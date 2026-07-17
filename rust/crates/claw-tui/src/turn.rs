//! Running a real turn for the full-screen frontend.
//!
//! `run_turn` is blocking and the draw loop must keep painting, so a turn runs
//! on a worker thread and reports back over a channel. The engine renders
//! through [`ChannelSink`], which translates its semantic events into the
//! frontend's [`StreamEvent`]s instead of writing to a terminal.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use claw_engine::{EngineClient, EngineToolExecutor, SinkResult, TurnSink};
use runtime::permission_enforcer::PermissionEnforcer;
use runtime::{ConversationRuntime, PermissionMode, PermissionPolicy, Session, TokenUsage};
use tools::GlobalToolRegistry;

use crate::app::StreamEvent;

/// Forwards engine events to the draw loop.
///
/// Sends are best-effort: if the receiver is gone the frontend has quit, and a
/// half-finished turn should not be an error.
pub struct ChannelSink {
    tx: Sender<StreamEvent>,
    /// Reasoning arrives as deltas; the frontend opens one block per run of
    /// them, mirroring how the REPL announces a thinking block once.
    thinking_open: bool,
    assistant_open: bool,
}

impl ChannelSink {
    pub fn new(tx: Sender<StreamEvent>) -> Self {
        Self {
            tx,
            thinking_open: false,
            assistant_open: false,
        }
    }

    fn send(&self, event: StreamEvent) {
        let _ = self.tx.send(event);
    }

    fn close_thinking(&mut self) {
        if self.thinking_open {
            self.thinking_open = false;
            self.send(StreamEvent::ThinkingEnd);
        }
    }
}

impl TurnSink for ChannelSink {
    fn text_delta(&mut self, text: &str) -> SinkResult {
        self.close_thinking();
        if !self.assistant_open {
            self.assistant_open = true;
            self.send(StreamEvent::AssistantStart);
        }
        self.send(StreamEvent::TextDelta(text.to_string()));
        Ok(())
    }

    fn text_block(&mut self, text: &str) -> SinkResult {
        self.text_delta(text)
    }

    fn thinking_delta(&mut self, thinking: &str) -> SinkResult {
        if !self.thinking_open {
            self.thinking_open = true;
            self.send(StreamEvent::ThinkingStart);
        }
        self.send(StreamEvent::ThinkingDelta(thinking.to_string()));
        Ok(())
    }

    fn thinking_block(&mut self, thinking: &str, _signature: Option<&str>) -> SinkResult {
        self.send(StreamEvent::ThinkingStart);
        self.send(StreamEvent::ThinkingDelta(thinking.to_string()));
        self.send(StreamEvent::ThinkingEnd);
        Ok(())
    }

    fn redacted_thinking(&mut self) -> SinkResult {
        self.send(StreamEvent::ThinkingStart);
        self.send(StreamEvent::ThinkingDelta(
            "(reasoning hidden by provider)".to_string(),
        ));
        self.send(StreamEvent::ThinkingEnd);
        Ok(())
    }

    fn tool_call(&mut self, name: &str, input: &str) -> SinkResult {
        self.close_thinking();
        self.assistant_open = false;
        self.send(StreamEvent::ToolStart {
            name: name.to_string(),
            detail: summarize_tool_input(input),
        });
        Ok(())
    }

    fn tool_result(&mut self, _name: &str, output: &str, is_error: bool) -> SinkResult {
        let prefix = if is_error { "✗" } else { "✓" };
        self.send(StreamEvent::ToolOutput(format!(
            "{prefix} {}",
            first_line(output, 200)
        )));
        Ok(())
    }

    fn block_stop(&mut self) -> SinkResult {
        self.close_thinking();
        Ok(())
    }

    fn usage(&mut self, usage: TokenUsage) -> SinkResult {
        self.send(StreamEvent::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_cents: 0,
        });
        Ok(())
    }

    fn turn_end(&mut self) -> SinkResult {
        self.close_thinking();
        Ok(())
    }
}

/// One-line preview of a tool's JSON input for the activity row.
fn summarize_tool_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Prefer the most identifying field rather than dumping raw JSON.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["path", "file_path", "command", "pattern", "query"] {
            if let Some(found) = value.get(key).and_then(serde_json::Value::as_str) {
                return truncate(found, 120);
            }
        }
    }
    truncate(trimmed, 120)
}

fn first_line(value: &str, limit: usize) -> String {
    let line = value.lines().next().unwrap_or("").trim();
    truncate(line, limit)
}

fn truncate(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Everything a turn needs that the frontend resolves once per send.
pub struct TurnRequest {
    pub prompt: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub session: Session,
    pub cwd: PathBuf,
}

/// What a finished turn hands back to the frontend.
///
/// The session comes back even when the turn fails: a provider error must not
/// cost the user their conversation history.
pub struct TurnOutcome {
    pub session: Option<Session>,
    pub error: Option<String>,
}

/// Build the engine runtime and run one turn to completion.
pub fn run_turn(request: TurnRequest, tx: &Sender<StreamEvent>) -> TurnOutcome {
    let TurnRequest {
        prompt,
        model,
        permission_mode,
        session,
        cwd,
    } = request;

    let policy = PermissionPolicy::new(permission_mode);
    let tool_registry =
        GlobalToolRegistry::builtin().with_enforcer(PermissionEnforcer::new(policy.clone()));

    let system_prompt = match runtime::load_system_prompt(
        cwd,
        current_date(),
        std::env::consts::OS,
        "",
        api::model_family_identity_for(&model),
    ) {
        Ok(sections) => sections,
        // Setup failed before the turn started: hand the session straight back.
        Err(error) => return TurnOutcome::failed(session, error.to_string()),
    };

    let session_id = session.session_id.clone();
    let client = match EngineClient::new(
        &session_id,
        model,
        true,
        None,
        tool_registry.clone(),
        Box::new(ChannelSink::new(tx.clone())),
    ) {
        Ok(client) => client,
        Err(error) => return TurnOutcome::failed(session, error.to_string()),
    };
    let executor =
        EngineToolExecutor::new(None, tool_registry, None, Box::new(claw_engine::NullSink));

    let mut runtime = ConversationRuntime::new(session, client, executor, policy, system_prompt);
    let error = runtime.run_turn(prompt, None).err().map(|e| e.to_string());
    TurnOutcome {
        session: Some(runtime.into_session()),
        error,
    }
}

impl TurnOutcome {
    fn failed(session: Session, error: String) -> Self {
        Self {
            session: Some(session),
            error: Some(error),
        }
    }
}

fn current_date() -> String {
    // The prompt only needs a stable date string; the frontend has no clock
    // dependency worth pulling in for this.
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::{first_line, summarize_tool_input, ChannelSink};
    use crate::app::StreamEvent;
    use claw_engine::TurnSink;
    use std::sync::mpsc;

    #[test]
    fn tool_input_summary_prefers_the_identifying_field_over_raw_json() {
        assert_eq!(
            summarize_tool_input(r#"{"path":"src/auth/token.rs","offset":4}"#),
            "src/auth/token.rs"
        );
        assert_eq!(
            summarize_tool_input(r#"{"command":"cargo test"}"#),
            "cargo test"
        );
    }

    #[test]
    fn tool_input_summary_falls_back_to_raw_text_for_unknown_shapes() {
        assert_eq!(summarize_tool_input(r#"{"weird":1}"#), r#"{"weird":1}"#);
        assert_eq!(summarize_tool_input(""), "");
    }

    #[test]
    fn first_line_collapses_multi_line_tool_output() {
        assert_eq!(first_line("ok\nsecond\nthird", 100), "ok");
        assert_eq!(first_line("", 100), "");
    }

    #[test]
    fn text_after_thinking_closes_the_reasoning_block_exactly_once() {
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx);

        sink.thinking_delta("pondering").expect("send");
        sink.text_delta("answer").expect("send");
        sink.text_delta(" continues").expect("send");
        drop(sink);

        let events: Vec<StreamEvent> = rx.iter().collect();
        assert!(matches!(events[0], StreamEvent::ThinkingStart));
        assert!(matches!(events[1], StreamEvent::ThinkingDelta(_)));
        assert!(matches!(events[2], StreamEvent::ThinkingEnd));
        assert!(matches!(events[3], StreamEvent::AssistantStart));
        // The second delta must not reopen the assistant block.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, StreamEvent::AssistantStart))
                .count(),
            1
        );
    }

    /// Drives a real turn against the configured provider. Opt-in because it
    /// needs credentials and network:
    ///   CLAW_TUI_LIVE_TEST=1 CLAW_TUI_LIVE_MODEL=<model> cargo test -p claw-tui -- --ignored
    #[test]
    #[ignore = "requires a configured provider and network"]
    fn live_turn_streams_assistant_text_from_the_configured_endpoint() {
        let Ok(model) = std::env::var("CLAW_TUI_LIVE_MODEL") else {
            panic!("set CLAW_TUI_LIVE_MODEL to the model to exercise");
        };
        let (tx, rx) = mpsc::channel();

        let outcome = super::run_turn(
            super::TurnRequest {
                prompt: "reply with exactly: TUILIVE".to_string(),
                model,
                permission_mode: runtime::PermissionMode::Prompt,
                session: runtime::Session::new(),
                cwd: std::env::current_dir().expect("cwd"),
            },
            &tx,
        );
        drop(tx);

        assert!(outcome.error.is_none(), "turn failed: {:?}", outcome.error);
        let text: String = rx
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            text.contains("TUILIVE"),
            "expected streamed assistant text, got {text:?}"
        );
        assert!(
            outcome
                .session
                .is_some_and(|session| !session.messages.is_empty()),
            "the turn must come back with conversation history"
        );
    }

    #[test]
    fn a_dead_receiver_does_not_fail_the_turn() {
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx);
        drop(rx);

        // The frontend quit mid-turn; the engine should not see an error.
        sink.text_delta("orphaned").expect("send must be lossy");
        sink.turn_end().expect("send must be lossy");
    }
}
