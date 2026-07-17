//! Running a real turn for the full-screen frontend.
//!
//! `run_turn` is blocking and the draw loop must keep painting, so a turn runs
//! on a worker thread and reports back over a channel. The engine renders
//! through [`ChannelSink`], which translates its semantic events into the
//! frontend's [`StreamEvent`]s instead of writing to a terminal.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use claw_engine::{
    EngineClient, EngineToolExecutor, RuntimeMcpState, RuntimePluginState, SinkResult, TurnSink,
};
use plugins::PluginRegistry;
use runtime::permission_enforcer::PermissionEnforcer;
use runtime::{
    ConversationRuntime, PermissionMode, PermissionPromptDecision, PermissionPrompter,
    PermissionRequest, Session, TokenUsage,
};

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
        // Streaming providers announce the block with an empty complete
        // block before sending its deltas. Do not render that placeholder;
        // the first real delta opens the single visible reasoning row.
        if thinking.is_empty() {
            return Ok(());
        }
        self.close_thinking();
        self.thinking_open = true;
        self.send(StreamEvent::ThinkingStart);
        self.send(StreamEvent::ThinkingDelta(thinking.to_string()));
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
pub(crate) fn summarize_tool_input(input: &str) -> String {
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

/// A tool call waiting on the user's approval.
///
/// The worker thread is blocked inside `run_turn` while this sits in the draw
/// loop's queue; answering it unblocks the turn.
pub struct PermissionAsk {
    pub tool_name: String,
    pub input: String,
    pub current_mode: PermissionMode,
    pub required_mode: PermissionMode,
    pub reason: Option<String>,
    pub reply: Sender<PermissionPromptDecision>,
}

/// Routes approval requests to the draw loop and waits for the answer.
struct ChannelPrompter {
    asks: Sender<PermissionAsk>,
}

impl PermissionPrompter for ChannelPrompter {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        let (reply, answer) = std::sync::mpsc::channel();
        let ask = PermissionAsk {
            tool_name: request.tool_name.clone(),
            input: request.input.clone(),
            current_mode: request.current_mode,
            required_mode: request.required_mode,
            reason: request.reason.clone(),
            reply,
        };
        if self.asks.send(ask).is_err() {
            // The frontend is gone; denying is the safe way to end the turn.
            return PermissionPromptDecision::Deny {
                reason: "frontend closed before the approval was answered".to_string(),
            };
        }
        // Blocks this worker thread only. The draw loop keeps painting and
        // sends the decision back when the user answers.
        answer
            .recv()
            .unwrap_or_else(|_| PermissionPromptDecision::Deny {
                reason: format!("approval for '{}' was never answered", request.tool_name),
            })
    }
}

/// Everything a turn needs that the frontend resolves once per send.
pub struct TurnRequest {
    pub prompt: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub session: Session,
    pub cwd: PathBuf,
    /// Where tool-approval requests go while the turn is blocked on them.
    pub asks: Sender<PermissionAsk>,
}

/// What a finished turn hands back to the frontend.
///
/// The session comes back even when the turn fails: a provider error must not
/// cost the user their conversation history.
pub struct TurnOutcome {
    pub session: Option<Session>,
    pub error: Option<String>,
}

/// Owns the plugin and MCP lifecycle for one runtime.
///
/// MCP servers are child processes. Dropping this shuts them down, so a turn
/// that fails partway through cannot leak them, mirroring the REPL's
/// `BuiltRuntime::drop`.
struct RuntimeLifecycle {
    plugin_registry: PluginRegistry,
    plugins_active: bool,
    mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    mcp_active: bool,
}

impl RuntimeLifecycle {
    fn new(
        plugin_registry: PluginRegistry,
        mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    ) -> Self {
        Self {
            plugin_registry,
            plugins_active: false,
            mcp_state,
            mcp_active: true,
        }
    }

    /// Run plugin `initialize` hooks, arming the matching shutdown.
    fn initialize_plugins(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.plugin_registry.initialize()?;
        self.plugins_active = true;
        Ok(())
    }

    fn shutdown_mcp(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.mcp_active {
            if let Some(mcp_state) = &self.mcp_state {
                mcp_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .shutdown()?;
            }
            self.mcp_active = false;
        }
        Ok(())
    }

    fn shutdown_plugins(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.plugins_active {
            self.plugin_registry.shutdown()?;
            self.plugins_active = false;
        }
        Ok(())
    }
}

impl Drop for RuntimeLifecycle {
    fn drop(&mut self) {
        let _ = self.shutdown_mcp();
        let _ = self.shutdown_plugins();
    }
}

/// Build the engine runtime and run one turn to completion.
///
/// The runtime is rebuilt per turn, so everything it resolves from
/// configuration — plugins, hooks, MCP servers — is resolved per turn too.
pub fn run_turn(request: TurnRequest, tx: &Sender<StreamEvent>) -> TurnOutcome {
    let TurnRequest {
        prompt,
        model,
        permission_mode,
        session,
        cwd,
        asks,
    } = request;

    let loader = runtime::ConfigLoader::default_for(&cwd);
    let runtime_config = match loader.load() {
        Ok(config) => config,
        // Setup failed before the turn started: hand the session straight back.
        Err(error) => return TurnOutcome::failed(session, error.to_string()),
    };
    let RuntimePluginState {
        feature_config,
        tool_registry,
        plugin_registry,
        mcp_state,
    } = match claw_engine::build_runtime_plugin_state(&cwd, &loader, &runtime_config) {
        Ok(state) => state,
        Err(error) => return TurnOutcome::failed(session, error.to_string()),
    };

    // From here on MCP servers may be running: the guard must own them so every
    // early return still shuts them down.
    let mut lifecycle = RuntimeLifecycle::new(plugin_registry, mcp_state.clone());
    if let Err(error) = lifecycle.initialize_plugins() {
        return TurnOutcome::failed(session, error.to_string());
    }

    // Derived from the registry before the enforcer is attached: the policy must
    // know every plugin and MCP tool's declared requirement, plus the user's
    // configured permission rules.
    let policy =
        match claw_engine::permission_policy(permission_mode, &feature_config, &tool_registry) {
            Ok(policy) => policy,
            Err(error) => return TurnOutcome::failed(session, error),
        };
    let tool_registry = tool_registry.with_enforcer(PermissionEnforcer::new(policy.clone()));

    let system_prompt = match runtime::load_system_prompt(
        cwd,
        BUILD_DATE,
        std::env::consts::OS,
        "",
        api::model_family_identity_for(&model),
    ) {
        Ok(sections) => sections,
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
    let executor = EngineToolExecutor::new(
        None,
        tool_registry,
        mcp_state,
        // Must be a ChannelSink, not NullSink: tool results are drawn from
        // TurnSink::tool_result, so a null sink silently swallows every tool's
        // output and the transcript stalls after "running".
        Box::new(ChannelSink::new(tx.clone())),
    );

    // `new_with_features`, not `new`: `new` uses RuntimeFeatureConfig::default(),
    // which silently drops the user's configured hooks.
    let mut runtime = ConversationRuntime::new_with_features(
        session,
        client,
        executor,
        policy,
        system_prompt,
        &feature_config,
    )
    .with_caveman_compression(runtime::caveman_enabled());
    let mut prompter = ChannelPrompter { asks };
    let error = runtime
        .run_turn(prompt, Some(&mut prompter))
        .err()
        .map(|e| e.to_string());
    let session = runtime.into_session();
    drop(lifecycle);
    TurnOutcome {
        session: Some(session),
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

/// Build-stamped date for the system prompt, matching the CLI's DEFAULT_DATE.
/// Both fall back to "unknown" when BUILD_DATE is not set at compile time.
const BUILD_DATE: &str = match option_env!("BUILD_DATE") {
    Some(date) => date,
    None => "unknown",
};

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

    #[test]
    fn empty_thinking_block_does_not_duplicate_the_following_reasoning_delta() {
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx);

        sink.thinking_block("", None).expect("send");
        sink.thinking_delta("actual reasoning").expect("send");
        sink.text_delta("answer").expect("send");
        drop(sink);

        let events: Vec<StreamEvent> = rx.iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingStart))
                .count(),
            1,
            "an empty stream-start block must not create a duplicate reasoning row"
        );
        assert!(matches!(
            events.as_slice(),
            [
                StreamEvent::ThinkingStart,
                StreamEvent::ThinkingDelta(text),
                StreamEvent::ThinkingEnd,
                StreamEvent::AssistantStart,
                StreamEvent::TextDelta(_),
            ] if text == "actual reasoning"
        ));
    }

    #[test]
    fn complete_thinking_block_and_deltas_share_one_reasoning_entry() {
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx);

        sink.thinking_block("initial reasoning", None)
            .expect("send");
        sink.thinking_delta(" and more").expect("send");
        sink.block_stop().expect("send");
        drop(sink);

        let events: Vec<StreamEvent> = rx.iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingStart))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingEnd))
                .count(),
            1
        );
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["initial reasoning", " and more"]);
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
                prompt: std::env::var("CLAW_TUI_LIVE_PROMPT")
                    .unwrap_or_else(|_| "reply with exactly: TUILIVE".to_string()),
                model,
                permission_mode: runtime::PermissionMode::Prompt,
                session: runtime::Session::new(),
                cwd: std::env::current_dir().expect("cwd"),
                asks: mpsc::channel().0,
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
        let expected =
            std::env::var("CLAW_TUI_LIVE_EXPECT").unwrap_or_else(|_| "TUILIVE".to_string());
        assert!(
            text.contains(&expected),
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
    fn an_approved_tool_unblocks_the_waiting_turn() {
        use runtime::{PermissionMode, PermissionPrompter, PermissionRequest};

        let (asks_tx, asks_rx) = mpsc::channel();
        let mut prompter = super::ChannelPrompter { asks: asks_tx };

        // The prompter blocks, so the "frontend" answers from another thread.
        let responder = std::thread::spawn(move || {
            let ask = asks_rx.recv().expect("an ask should arrive");
            assert_eq!(ask.tool_name, "Bash");
            ask.reply
                .send(runtime::PermissionPromptDecision::Allow)
                .expect("reply should reach the blocked turn");
        });

        let decision = prompter.decide(&PermissionRequest {
            tool_name: "Bash".to_string(),
            input: r#"{"command":"ls"}"#.to_string(),
            current_mode: PermissionMode::Prompt,
            required_mode: PermissionMode::DangerFullAccess,
            reason: None,
        });

        responder.join().expect("responder");
        assert_eq!(decision, runtime::PermissionPromptDecision::Allow);
    }

    #[test]
    fn a_frontend_that_quits_mid_prompt_denies_instead_of_hanging() {
        use runtime::{PermissionMode, PermissionPrompter, PermissionRequest};

        // Receiver dropped: the draw loop is gone. The worker must not block
        // forever waiting for an answer that can never come.
        let (asks_tx, asks_rx) = mpsc::channel();
        drop(asks_rx);
        let mut prompter = super::ChannelPrompter { asks: asks_tx };

        let decision = prompter.decide(&PermissionRequest {
            tool_name: "Bash".to_string(),
            input: "{}".to_string(),
            current_mode: PermissionMode::Prompt,
            required_mode: PermissionMode::DangerFullAccess,
            reason: None,
        });

        assert!(matches!(
            decision,
            runtime::PermissionPromptDecision::Deny { .. }
        ));
    }

    #[test]
    fn a_dropped_reply_channel_denies_rather_than_blocking_forever() {
        use runtime::{PermissionMode, PermissionPrompter, PermissionRequest};

        let (asks_tx, asks_rx) = mpsc::channel();
        let mut prompter = super::ChannelPrompter { asks: asks_tx };

        // The frontend takes the ask but dies before answering.
        let responder = std::thread::spawn(move || {
            let ask = asks_rx.recv().expect("an ask should arrive");
            drop(ask);
        });

        let decision = prompter.decide(&PermissionRequest {
            tool_name: "read_file".to_string(),
            input: "{}".to_string(),
            current_mode: PermissionMode::Prompt,
            required_mode: PermissionMode::WorkspaceWrite,
            reason: None,
        });

        responder.join().expect("responder");
        assert!(matches!(
            decision,
            runtime::PermissionPromptDecision::Deny { .. }
        ));
    }

    #[test]
    fn tool_results_reach_the_transcript() {
        // Regression: the executor was built with NullSink, so tools showed as
        // "running" and their output never arrived.
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx);

        sink.tool_call("read_file", r#"{"path":"src/lib.rs"}"#)
            .expect("send");
        sink.tool_result("read_file", "48 lines read\nmore detail", false)
            .expect("send");
        sink.tool_result("write_file", "permission denied", true)
            .expect("send");
        drop(sink);

        let events: Vec<StreamEvent> = rx.iter().collect();
        let outputs: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolOutput(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            outputs,
            vec!["✓ 48 lines read", "✗ permission denied"],
            "tool output must be forwarded, collapsed to one line, error-marked"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolStart { name, .. } if name == "read_file")));
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

/// What the frontend resolves from `settings.json` before a turn.
///
/// `run_turn` reads the user's configuration, so these cover the difference
/// between honouring it and ignoring it. They exercise the same
/// `claw_engine::setup` entry points `run_turn` calls, without needing a
/// provider or network.
#[cfg(test)]
mod config_tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use runtime::{ConfigLoader, PermissionMode, PermissionOutcome, PermissionPolicy};

    /// A scratch directory that removes itself, so a failing assertion cannot
    /// leave fixtures behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::time::{SystemTime, UNIX_EPOCH};

            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time should be after epoch")
                .as_nanos();
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("claw-tui-{label}-{nanos}-{unique}"));
            fs::create_dir_all(&path).expect("temp dir should be creatable");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Resolve configuration exactly as `run_turn` does, for an isolated
    /// workspace and config home.
    fn plugin_state_for(
        workspace: &Path,
        config_home: &Path,
        settings: &str,
    ) -> claw_engine::RuntimePluginState {
        fs::write(config_home.join("settings.json"), settings).expect("write settings");
        let loader = ConfigLoader::new(workspace, config_home);
        let runtime_config = loader.load().expect("runtime config should load");
        claw_engine::build_runtime_plugin_state(workspace, &loader, &runtime_config)
            .expect("runtime plugin state should build")
    }

    fn write_plugin_fixture(root: &Path, name: &str) {
        fs::create_dir_all(root.join(".claude-plugin")).expect("manifest dir");
        fs::write(root.join("run.sh"), "#!/bin/sh\nprintf 'plugin tool'\n").expect("write tool");
        fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            format!(
                r#"{{
                  "name": "{name}",
                  "version": "1.0.0",
                  "description": "tui plugin fixture",
                  "tools": [
                    {{
                      "name": "tui_fixture_tool",
                      "description": "fixture tool",
                      "inputSchema": {{ "type": "object" }},
                      "command": "./run.sh",
                      "requiredPermission": "workspace-write"
                    }}
                  ]
                }}"#
            ),
        )
        .expect("write plugin manifest");
    }

    #[test]
    fn hooks_configured_in_settings_reach_the_runtime_feature_config() {
        // The frontend used to build its runtime with RuntimeFeatureConfig's
        // default, so configured hooks never fired.
        let workspace = TempDir::new("hooks-workspace");
        let config_home = TempDir::new("hooks-config");
        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            r#"{"hooks":{"PreToolUse":["./guard.sh"],"PostToolUse":["./audit.sh"]}}"#,
        );

        assert_eq!(
            state.feature_config.hooks().pre_tool_use(),
            vec!["./guard.sh".to_string()],
            "a configured PreToolUse hook must reach the runtime"
        );
        assert_eq!(
            state.feature_config.hooks().post_tool_use(),
            vec!["./audit.sh".to_string()]
        );
    }

    #[test]
    fn deny_rules_configured_in_settings_block_a_tool_the_bare_mode_would_allow() {
        let workspace = TempDir::new("deny-workspace");
        let config_home = TempDir::new("deny-config");
        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            r#"{"permissions":{"deny":["Bash(rm -rf:*)"]}}"#,
        );

        let policy = claw_engine::permission_policy(
            PermissionMode::DangerFullAccess,
            &state.feature_config,
            &state.tool_registry,
        )
        .expect("permission policy should build");

        let outcome = policy.authorize("bash", r#"{"command":"rm -rf /tmp/x"}"#, None);
        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "a configured deny rule must win over the permission mode, got {outcome:?}"
        );

        // The mode alone would have allowed it: this is the gap the rule closes.
        let bare = PermissionPolicy::new(PermissionMode::DangerFullAccess);
        assert!(!matches!(
            bare.authorize("bash", r#"{"command":"rm -rf /tmp/x"}"#, None),
            PermissionOutcome::Deny { .. }
        ));
    }

    #[test]
    fn the_derived_policy_knows_each_tools_declared_permission_requirement() {
        let workspace = TempDir::new("requirement-workspace");
        let config_home = TempDir::new("requirement-config");
        let state = plugin_state_for(workspace.path(), config_home.path(), "{}");

        let policy = claw_engine::permission_policy(
            PermissionMode::Prompt,
            &state.feature_config,
            &state.tool_registry,
        )
        .expect("permission policy should build");

        assert_eq!(
            policy.required_mode_for("read_file"),
            PermissionMode::ReadOnly,
            "reading a file must not be treated as full access"
        );
        // A bare policy has no tool requirements and falls back to the strictest
        // answer for everything.
        assert_eq!(
            PermissionPolicy::new(PermissionMode::Prompt).required_mode_for("read_file"),
            PermissionMode::DangerFullAccess
        );
    }

    #[test]
    fn plugin_tools_are_registered_and_carry_their_declared_permission() {
        let workspace = TempDir::new("plugin-workspace");
        let config_home = TempDir::new("plugin-config");
        let plugin_dir = TempDir::new("plugin-source");
        write_plugin_fixture(&plugin_dir.path().join("tui-demo"), "tui-demo");

        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            &format!(
                r#"{{
                  "enabledPlugins": {{ "tui-demo@external": true }},
                  "plugins": {{ "externalDirectories": ["{}"] }}
                }}"#,
                plugin_dir.path().to_string_lossy()
            ),
        );

        assert!(
            state
                .tool_registry
                .actual_tool_names()
                .contains(&"tui_fixture_tool".to_string()),
            "a configured plugin's tool must be registered, got {:?}",
            state.tool_registry.actual_tool_names()
        );

        let policy = claw_engine::permission_policy(
            PermissionMode::Prompt,
            &state.feature_config,
            &state.tool_registry,
        )
        .expect("permission policy should build");
        assert_eq!(
            policy.required_mode_for("tui_fixture_tool"),
            PermissionMode::WorkspaceWrite,
            "the plugin tool's declared requirement must reach the policy"
        );
    }

    #[test]
    fn no_configured_mcp_servers_means_no_mcp_state_and_no_startup_cost() {
        let workspace = TempDir::new("nomcp-workspace");
        let config_home = TempDir::new("nomcp-config");
        let state = plugin_state_for(workspace.path(), config_home.path(), "{}");

        assert!(
            state.mcp_state.is_none(),
            "a session without MCP servers must not start an MCP reactor"
        );
        assert!(!state
            .tool_registry
            .actual_tool_names()
            .contains(&"MCPTool".to_string()));
    }

    #[test]
    fn a_configured_mcp_server_produces_mcp_state_and_its_wrapper_tools() {
        let workspace = TempDir::new("mcp-workspace");
        let config_home = TempDir::new("mcp-config");
        // The server never comes up; discovery is best-effort, so the session
        // still gets MCP state and reports the server as pending.
        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            r#"{"mcpServers":{"alpha":{"command":"claw-tui-no-such-mcp-binary","args":[]}}}"#,
        );

        let mcp_state = state
            .mcp_state
            .as_ref()
            .expect("a configured MCP server must produce MCP state");
        {
            let mut guard = mcp_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(guard.server_names(), vec!["alpha".to_string()]);
            assert_eq!(guard.pending_servers(), Some(vec!["alpha".to_string()]));
            guard.shutdown().expect("mcp shutdown should succeed");
        }

        let names = state.tool_registry.actual_tool_names();
        for wrapper in ["MCPTool", "ListMcpResourcesTool", "ReadMcpResourceTool"] {
            assert!(
                names.contains(&wrapper.to_string()),
                "configuring a server must expose {wrapper}, got {names:?}"
            );
        }
    }

    #[test]
    fn the_runtime_lifecycle_shuts_mcp_down_when_the_turn_ends() {
        use super::RuntimeLifecycle;

        let workspace = TempDir::new("lifecycle-workspace");
        let config_home = TempDir::new("lifecycle-config");
        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            r#"{"mcpServers":{"alpha":{"command":"claw-tui-no-such-mcp-binary","args":[]}}}"#,
        );
        let mcp_state = state.mcp_state.clone().expect("mcp state");

        // Dropping the guard is what a finished (or failed) turn does.
        let lifecycle = RuntimeLifecycle::new(state.plugin_registry, state.mcp_state);
        drop(lifecycle);

        // Shutdown ran once already; asking again must be a no-op, not a panic
        // or a double-shutdown error.
        mcp_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown()
            .expect("a second shutdown must stay harmless");
    }
}
