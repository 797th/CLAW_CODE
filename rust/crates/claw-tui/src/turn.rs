//! Running turns for the full-screen frontend.
//!
//! A turn blocks and executes tools, so it cannot share the draw thread. One
//! long-lived *session worker* thread owns the [`ConversationRuntime`] and
//! receives prompts over a channel; the draw loop keeps painting and reads the
//! resulting [`StreamEvent`]s. The engine renders through [`ChannelSink`],
//! which translates its semantic events into the frontend's `StreamEvent`s
//! instead of writing to a terminal.
//!
//! The runtime is *session*-scoped, not turn-scoped: building it loads config,
//! initializes plugins and starts every configured MCP server, which the REPL
//! pays once per session. It is rebuilt only when [`RuntimeKey`] changes — see
//! [`SessionRuntime::ensure_engine`].
//!
//! The runtime is pinned inside the worker thread and never moves; only the
//! prompt in and the events out cross a thread boundary.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claw_engine::{
    EngineClient, EngineToolExecutor, RuntimeMcpState, RuntimePluginState, SinkResult, TurnSink,
};
use plugins::PluginRegistry;
use runtime::permission_enforcer::PermissionEnforcer;
use runtime::{
    pricing_for_model, ConversationRuntime, ModelPricing, PermissionMode, PermissionPromptDecision,
    PermissionPrompter, PermissionRequest, Session, TokenUsage,
};

use crate::app::StreamEvent;

/// How long a quitting frontend waits for the worker to stop before giving up.
///
/// The worker only notices a shutdown *between* turns, so a turn already in
/// flight has to end first. Waiting forever would hang the exit behind a slow
/// provider call, so the wait is bounded and the process leaves any remaining
/// MCP children to the OS.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
    /// Resolved once per sink: usage arrives priced, because the status bar has
    /// no way to look a model up on its own.
    pricing: ModelPricing,
}

impl ChannelSink {
    pub fn new(tx: Sender<StreamEvent>, model: &str) -> Self {
        Self {
            tx,
            thinking_open: false,
            assistant_open: false,
            pricing: pricing_for_model_or_default(model),
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

/// Pricing for `model`, falling back to the REPL's default tier.
///
/// An unknown or misspelled model must still cost *something* rather than
/// silently reporting free, which is what a hardcoded zero did.
fn pricing_for_model_or_default(model: &str) -> ModelPricing {
    pricing_for_model(model).unwrap_or_else(ModelPricing::default_sonnet_tier)
}

/// Whole cents for `usage` at `pricing`, rounded to the nearest cent.
///
/// Saturating rather than panicking: a bad price must not take the status bar
/// down with it.
fn cost_cents(usage: TokenUsage, pricing: ModelPricing) -> u32 {
    let cents = usage
        .estimate_cost_usd_with_pricing(pricing)
        .total_cost_usd()
        * 100.0;
    if cents.is_finite() && cents > 0.0 {
        cents.round().min(f64::from(u32::MAX)) as u32
    } else {
        0
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
            cost_cents: cost_cents(usage, self.pricing),
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
/// The session worker is blocked inside the turn while this sits in the draw
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
        let (reply, answer) = mpsc::channel();
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
        // Blocks the session worker only. The draw loop keeps painting and
        // sends the decision back when the user answers.
        answer
            .recv()
            .unwrap_or_else(|_| PermissionPromptDecision::Deny {
                reason: format!("approval for '{}' was never answered", request.tool_name),
            })
    }
}

/// The settings a built runtime is bound to.
///
/// Both can change between turns (`/model`, Shift+Tab mode cycling) and neither
/// can be swapped inside a built runtime: the model is baked into the engine
/// client and the mode into the permission policy *and* the enforcer attached
/// to the tool registry. A change therefore forces a rebuild — the one case
/// where the frontend pays plugin init and MCP startup again mid-session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeKey {
    pub model: String,
    pub permission_mode: PermissionMode,
}

/// One turn's worth of engine, for as long as its [`RuntimeKey`] holds.
trait TurnEngine {
    /// Run one turn to completion, returning what it consumed.
    fn run_turn(&mut self, prompt: String) -> Result<TokenUsage, String>;

    /// The conversation so far, for persistence between turns.
    fn session(&self) -> &Session;

    /// Give the conversation up, tearing the engine down around it.
    fn into_session(self) -> Session;
}

/// Builds a [`TurnEngine`]. Injected so the rebuild policy can be proven
/// without a provider, credentials or MCP servers.
trait EngineFactory {
    type Engine: TurnEngine;

    fn build(&mut self, key: &RuntimeKey, session: Session) -> Result<Self::Engine, String>;
}

/// Owns the plugin and MCP lifecycle for one runtime.
///
/// MCP servers are child processes. Dropping this shuts them down, so a session
/// that fails partway through building cannot leak them, mirroring the REPL's
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

/// The real engine: a `ConversationRuntime` plus the MCP and plugin processes
/// it was built against.
struct EngineRuntime {
    /// Declared before `lifecycle` so the runtime — and the tool registry
    /// holding MCP handles — is dropped before the servers are stopped.
    runtime: ConversationRuntime<EngineClient, EngineToolExecutor>,
    asks: Sender<PermissionAsk>,
    lifecycle: RuntimeLifecycle,
}

impl TurnEngine for EngineRuntime {
    fn run_turn(&mut self, prompt: String) -> Result<TokenUsage, String> {
        let mut prompter = ChannelPrompter {
            asks: self.asks.clone(),
        };
        self.runtime
            .run_turn(prompt, Some(&mut prompter))
            .map(|summary| summary.usage)
            .map_err(|error| error.to_string())
    }

    fn session(&self) -> &Session {
        self.runtime.session()
    }

    fn into_session(self) -> Session {
        let Self {
            runtime, lifecycle, ..
        } = self;
        let session = runtime.into_session();
        // Explicit, not incidental: the servers stop here, before the caller
        // builds their replacements.
        drop(lifecycle);
        session
    }
}

/// Builds the engine the frontend actually runs, from the user's configuration.
struct EngineRuntimeFactory {
    cwd: PathBuf,
    events: Sender<StreamEvent>,
    asks: Sender<PermissionAsk>,
}

impl EngineFactory for EngineRuntimeFactory {
    type Engine = EngineRuntime;

    fn build(&mut self, key: &RuntimeKey, session: Session) -> Result<EngineRuntime, String> {
        let loader = runtime::ConfigLoader::default_for(&self.cwd);
        let runtime_config = loader.load().map_err(|error| error.to_string())?;
        let RuntimePluginState {
            feature_config,
            tool_registry,
            plugin_registry,
            mcp_state,
        } = claw_engine::build_runtime_plugin_state(&self.cwd, &loader, &runtime_config)
            .map_err(|error| error.to_string())?;

        // From here on MCP servers may be running: the guard must own them so
        // every early return still shuts them down.
        let mut lifecycle = RuntimeLifecycle::new(plugin_registry, mcp_state.clone());
        lifecycle
            .initialize_plugins()
            .map_err(|error| error.to_string())?;

        // Derived from the registry before the enforcer is attached: the policy
        // must know every plugin and MCP tool's declared requirement, plus the
        // user's configured permission rules.
        let policy =
            claw_engine::permission_policy(key.permission_mode, &feature_config, &tool_registry)?;
        let tool_registry = tool_registry.with_enforcer(PermissionEnforcer::new(policy.clone()));

        let system_prompt = runtime::load_system_prompt(
            &self.cwd,
            BUILD_DATE,
            std::env::consts::OS,
            "",
            api::model_family_identity_for(&key.model),
        )
        .map_err(|error| error.to_string())?;

        let client = EngineClient::new(
            &session.session_id,
            key.model.clone(),
            true,
            None,
            tool_registry.clone(),
            Box::new(ChannelSink::new(self.events.clone(), &key.model)),
        )
        .map_err(|error| error.to_string())?;
        let executor = EngineToolExecutor::new(
            None,
            tool_registry,
            mcp_state,
            // Must be a ChannelSink, not NullSink: tool results are drawn from
            // TurnSink::tool_result, so a null sink silently swallows every
            // tool's output and the transcript stalls after "running".
            Box::new(ChannelSink::new(self.events.clone(), &key.model)),
        );

        // `new_with_features`, not `new`: `new` uses RuntimeFeatureConfig::default(),
        // which silently drops the user's configured hooks.
        let runtime = ConversationRuntime::new_with_features(
            session,
            client,
            executor,
            policy,
            system_prompt,
            &feature_config,
        )
        .with_caveman_compression(runtime::caveman_enabled());

        Ok(EngineRuntime {
            runtime,
            asks: self.asks.clone(),
            lifecycle,
        })
    }
}

/// The engine currently built, and what it was built for.
struct ActiveEngine<E> {
    key: RuntimeKey,
    engine: E,
}

/// A session's engine, rebuilt only when it has to be.
///
/// This is what makes the runtime session-scoped: `ensure_engine` is the only
/// path to an engine, and it reuses the built one whenever the [`RuntimeKey`]
/// still matches.
struct SessionRuntime<F: EngineFactory> {
    factory: F,
    active: Option<ActiveEngine<F::Engine>>,
    /// History while no engine holds it: before the first turn, and whenever a
    /// build failed. Exactly one of `active`/`parked` carries it.
    parked: Option<Session>,
}

impl<F: EngineFactory> SessionRuntime<F> {
    fn new(factory: F, session: Session) -> Self {
        Self {
            factory,
            active: None,
            parked: Some(session),
        }
    }

    fn session(&self) -> Option<&Session> {
        self.active
            .as_ref()
            .map(|active| active.engine.session())
            .or(self.parked.as_ref())
    }

    /// The engine for `key`, building one only if the built one does not match.
    ///
    /// Reuse is the point: a build loads config, runs plugin `initialize` hooks
    /// and starts every configured MCP server. Doing that per turn is the cost
    /// this type exists to avoid.
    fn ensure_engine(&mut self, key: &RuntimeKey) -> Result<&mut F::Engine, String> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.key == *key)
        {
            return Ok(&mut self
                .active
                .as_mut()
                .expect("the key check just proved an engine is built")
                .engine);
        }

        // Either the first turn, or the model/permission mode changed. Take the
        // history out of the old engine so a rebuild keeps the conversation,
        // and let it go — stopping its MCP servers — before starting new ones.
        let session = match self.active.take() {
            Some(active) => active.engine.into_session(),
            None => self.parked.take().unwrap_or_default(),
        };
        // Park a copy first: a failed build must not cost the user their
        // history.
        self.parked = Some(session.clone());
        let engine = self.factory.build(key, session)?;
        self.parked = None;
        Ok(&mut self
            .active
            .insert(ActiveEngine {
                key: key.clone(),
                engine,
            })
            .engine)
    }

    fn run_turn(&mut self, key: &RuntimeKey, prompt: String) -> TurnOutcome {
        match self.ensure_engine(key) {
            Ok(engine) => match engine.run_turn(prompt) {
                Ok(usage) => TurnOutcome {
                    usage: Some(usage),
                    error: None,
                },
                // A failed turn keeps the engine: the provider erred, the
                // session did not, and rebuilding would restart MCP for nothing.
                Err(error) => TurnOutcome {
                    usage: None,
                    error: Some(error),
                },
            },
            Err(error) => TurnOutcome {
                usage: None,
                error: Some(error),
            },
        }
    }

    fn into_session(self) -> Session {
        match self.active {
            Some(active) => active.engine.into_session(),
            None => self.parked.unwrap_or_default(),
        }
    }
}

/// What a finished turn reports back.
struct TurnOutcome {
    usage: Option<TokenUsage>,
    error: Option<String>,
}

/// Writes the session where the REPL writes it, after every turn.
///
/// Same store and same format as `rusty-claude-cli`, so `clawcli --resume` sees
/// frontend sessions and the frontend sees the REPL's.
struct SessionPersistence {
    /// `None` once saving has failed: the user has been told, and repeating the
    /// warning every turn would bury the transcript.
    path: Option<PathBuf>,
}

impl SessionPersistence {
    /// Bind `session` to a file in the workspace's session store, and write it
    /// once so `--resume` can see it before the first turn finishes.
    fn attach(session: Session, cwd: &Path, events: &Sender<StreamEvent>) -> (Session, Self) {
        match persistence_path_for(cwd, &session.session_id) {
            Ok(path) => {
                let session = session.with_persistence_path(path.clone());
                let mut persistence = Self { path: Some(path) };
                persistence.save(&session, events);
                (session, persistence)
            }
            Err(error) => {
                notice(events, &format!("session will not be saved: {error}"));
                (session, Self { path: None })
            }
        }
    }

    fn save(&mut self, session: &Session, events: &Sender<StreamEvent>) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Err(error) = session.save_to_path(path) {
            notice(events, &format!("session was not saved: {error}"));
            self.path = None;
        }
    }
}

fn persistence_path_for(cwd: &Path, session_id: &str) -> Result<PathBuf, String> {
    let store = runtime::SessionStore::from_cwd(cwd).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(store.sessions_dir()).map_err(|error| error.to_string())?;
    Ok(store.create_handle(session_id).path)
}

fn notice(events: &Sender<StreamEvent>, text: &str) {
    let _ = events.send(StreamEvent::Notice(text.to_string()));
}

/// What the draw loop asks the session worker to do.
enum SessionCommand {
    RunTurn {
        prompt: String,
        model: String,
        permission_mode: PermissionMode,
    },
    Shutdown,
}

/// The draw loop's end of a session worker.
///
/// Dropping it stops the worker, which is what shuts MCP servers and plugins
/// down on exit.
pub struct SessionHandle {
    commands: Sender<SessionCommand>,
    events: Receiver<StreamEvent>,
    /// The worker's final session, handed back when it stops.
    finished: Receiver<Session>,
}

impl SessionHandle {
    pub fn events(&self) -> &Receiver<StreamEvent> {
        &self.events
    }

    /// Ask for a turn. `false` means the worker is gone and no turn started.
    pub fn run_turn(&self, prompt: String, model: String, permission_mode: PermissionMode) -> bool {
        self.commands
            .send(SessionCommand::RunTurn {
                prompt,
                model,
                permission_mode,
            })
            .is_ok()
    }

    /// Stop the worker and take the conversation back.
    ///
    /// `None` if the worker did not stop within [`SHUTDOWN_GRACE`], or already
    /// stopped.
    pub fn shutdown(mut self) -> Option<Session> {
        self.stop()
    }

    fn stop(&mut self) -> Option<Session> {
        let _ = self.commands.send(SessionCommand::Shutdown);
        self.finished.recv_timeout(SHUTDOWN_GRACE).ok()
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Start a session: one thread, one runtime, many turns.
///
/// The runtime is built on the first turn rather than here, so a frontend that
/// never sends a message never pays for plugins or MCP.
pub fn spawn_session(cwd: PathBuf, asks: Sender<PermissionAsk>) -> SessionHandle {
    let (commands, command_rx) = mpsc::channel();
    let (event_tx, events) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let session = run_session(cwd, asks, &command_rx, &event_tx);
        // Best-effort: a frontend that gave up waiting is already gone.
        let _ = finished_tx.send(session);
    });
    SessionHandle {
        commands,
        events,
        finished,
    }
}

/// The session worker: owns the runtime for the whole session.
fn run_session(
    cwd: PathBuf,
    asks: Sender<PermissionAsk>,
    commands: &Receiver<SessionCommand>,
    events: &Sender<StreamEvent>,
) -> Session {
    let (session, mut persistence) =
        SessionPersistence::attach(Session::new().with_workspace_root(&cwd), &cwd, events);
    let factory = EngineRuntimeFactory {
        cwd,
        events: events.clone(),
        asks,
    };
    let mut session_runtime = SessionRuntime::new(factory, session);

    while let Ok(command) = commands.recv() {
        let SessionCommand::RunTurn {
            prompt,
            model,
            permission_mode,
        } = command
        else {
            break;
        };
        let key = RuntimeKey {
            model,
            permission_mode,
        };
        let outcome = session_runtime.run_turn(&key, prompt);
        // Persist whatever the turn left behind, including after a failure: the
        // user's history is worth more than the error.
        if let Some(session) = session_runtime.session() {
            persistence.save(session, events);
        }
        let event = match outcome.error {
            Some(error) => StreamEvent::Failed(error),
            None => {
                let usage = outcome.usage.unwrap_or_default();
                StreamEvent::Done {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cost_cents: cost_cents(usage, pricing_for_model_or_default(&key.model)),
                }
            }
        };
        let _ = events.send(event);
    }

    session_runtime.into_session()
}

/// Build-stamped date for the system prompt, matching the CLI's DEFAULT_DATE.
/// Both fall back to "unknown" when BUILD_DATE is not set at compile time.
const BUILD_DATE: &str = match option_env!("BUILD_DATE") {
    Some(date) => date,
    None => "unknown",
};

#[cfg(test)]
mod tests {
    use super::{
        cost_cents, first_line, pricing_for_model_or_default, summarize_tool_input, ChannelSink,
        EngineFactory, RuntimeKey, SessionRuntime, TurnEngine,
    };
    use crate::app::StreamEvent;
    use claw_engine::TurnSink;
    use runtime::{PermissionMode, Session, TokenUsage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;

    const MODEL: &str = "claude-sonnet-4-5";

    fn key(model: &str, permission_mode: PermissionMode) -> RuntimeKey {
        RuntimeKey {
            model: model.to_string(),
            permission_mode,
        }
    }

    /// The text of every message in `session`, in order.
    fn transcript(session: &Session) -> Vec<String> {
        session
            .messages
            .iter()
            .flat_map(|message| &message.blocks)
            .filter_map(|block| match block {
                runtime::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A `TurnEngine` that records turns in the session instead of calling a
    /// provider, so the rebuild policy can be tested without credentials.
    struct FakeEngine {
        session: Session,
        /// Turns that must fail, as a provider error would.
        fail: bool,
    }

    impl TurnEngine for FakeEngine {
        fn run_turn(&mut self, prompt: String) -> Result<TokenUsage, String> {
            if self.fail {
                return Err(format!("provider refused '{prompt}'"));
            }
            self.session
                .push_message(runtime::ConversationMessage::user_text(prompt))
                .expect("a fixture message should append");
            Ok(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                ..TokenUsage::default()
            })
        }

        fn session(&self) -> &Session {
            &self.session
        }

        fn into_session(self) -> Session {
            self.session
        }
    }

    /// Counts builds, which is the observable proof that the runtime is
    /// session-scoped rather than per turn.
    struct CountingFactory {
        builds: Arc<AtomicUsize>,
        keys: Arc<std::sync::Mutex<Vec<RuntimeKey>>>,
        fail_turns: bool,
        fail_build: bool,
    }

    impl CountingFactory {
        fn new() -> Self {
            Self {
                builds: Arc::new(AtomicUsize::new(0)),
                keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                fail_turns: false,
                fail_build: false,
            }
        }
    }

    impl EngineFactory for CountingFactory {
        type Engine = FakeEngine;

        fn build(&mut self, key: &RuntimeKey, session: Session) -> Result<FakeEngine, String> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.keys.lock().expect("keys").push(key.clone());
            if self.fail_build {
                return Err("runtime could not be built".to_string());
            }
            Ok(FakeEngine {
                session,
                fail: self.fail_turns,
            })
        }
    }

    #[test]
    fn the_runtime_is_built_once_for_a_whole_session_not_once_per_turn() {
        // Every build loads config, initializes plugins and starts every
        // configured MCP server. The frontend used to pay that per message.
        let factory = CountingFactory::new();
        let builds = Arc::clone(&factory.builds);
        let mut session = SessionRuntime::new(factory, Session::new());
        let key = key(MODEL, PermissionMode::Prompt);

        for turn in 0..5 {
            let outcome = session.run_turn(&key, format!("turn {turn}"));
            assert!(outcome.error.is_none(), "turn {turn}: {:?}", outcome.error);
        }

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "five turns with unchanged settings must share one runtime"
        );
    }

    #[test]
    fn changing_the_model_rebuilds_the_runtime_and_repeating_it_does_not() {
        let factory = CountingFactory::new();
        let builds = Arc::clone(&factory.builds);
        let keys = Arc::clone(&factory.keys);
        let mut session = SessionRuntime::new(factory, Session::new());

        session.run_turn(&key(MODEL, PermissionMode::Prompt), "first".to_string());
        session.run_turn(
            &key("claude-opus-4-6", PermissionMode::Prompt),
            "second".to_string(),
        );
        session.run_turn(
            &key("claude-opus-4-6", PermissionMode::Prompt),
            "third".to_string(),
        );

        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "the model change rebuilds once; staying on it must not rebuild again"
        );
        let models: Vec<String> = keys
            .lock()
            .expect("keys")
            .iter()
            .map(|key| key.model.clone())
            .collect();
        assert_eq!(models, vec![MODEL, "claude-opus-4-6"]);
    }

    #[test]
    fn changing_the_permission_mode_rebuilds_the_runtime_and_repeating_it_does_not() {
        // The mode is baked into the policy and into the registry's enforcer,
        // so it cannot be swapped inside a built runtime.
        let factory = CountingFactory::new();
        let builds = Arc::clone(&factory.builds);
        let mut session = SessionRuntime::new(factory, Session::new());

        session.run_turn(&key(MODEL, PermissionMode::Prompt), "first".to_string());
        session.run_turn(
            &key(MODEL, PermissionMode::DangerFullAccess),
            "second".to_string(),
        );
        session.run_turn(
            &key(MODEL, PermissionMode::DangerFullAccess),
            "third".to_string(),
        );

        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn history_accumulates_across_turns_and_survives_a_rebuild() {
        let factory = CountingFactory::new();
        let mut session = SessionRuntime::new(factory, Session::new());

        session.run_turn(&key(MODEL, PermissionMode::Prompt), "first".to_string());
        // A model switch tears the engine down and builds a new one; the
        // conversation must travel across.
        session.run_turn(
            &key("claude-opus-4-6", PermissionMode::Prompt),
            "second".to_string(),
        );

        assert_eq!(
            transcript(&session.into_session()),
            vec!["first", "second"],
            "a rebuild must carry the conversation across"
        );
    }

    #[test]
    fn a_failed_turn_keeps_the_session_and_its_history() {
        // A provider error must not cost the user their conversation.
        let mut factory = CountingFactory::new();
        factory.fail_turns = true;
        let builds = Arc::clone(&factory.builds);
        let mut runtime = SessionRuntime::new(factory, Session::new());

        let seeded = runtime
            .ensure_engine(&key(MODEL, PermissionMode::Prompt))
            .expect("engine should build");
        seeded.fail = false;
        seeded.run_turn("kept".to_string()).expect("first turn");
        seeded.fail = true;

        let outcome = runtime.run_turn(&key(MODEL, PermissionMode::Prompt), "doomed".to_string());

        assert!(outcome.error.is_some(), "the turn must report its failure");
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "a failed turn must not throw the runtime away"
        );
        assert_eq!(
            transcript(&runtime.into_session()),
            vec!["kept"],
            "history from before the failure must survive"
        );
    }

    #[test]
    fn a_failed_build_reports_the_error_and_leaves_the_history_intact() {
        let mut factory = CountingFactory::new();
        factory.fail_build = true;
        let mut runtime = SessionRuntime::new(factory, Session::new());

        let outcome = runtime.run_turn(&key(MODEL, PermissionMode::Prompt), "hello".to_string());

        assert_eq!(outcome.error.as_deref(), Some("runtime could not be built"));
        // The session is still there to be handed back, not consumed by the
        // build that failed.
        assert!(runtime.session().is_some());
        assert!(runtime.into_session().messages.is_empty());
    }

    #[test]
    fn cost_is_priced_from_the_model_and_an_unknown_model_still_costs_something() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..TokenUsage::default()
        };

        // Opus: $15/M in + $75/M out = $90.00 = 9000 cents.
        let opus = cost_cents(usage, pricing_for_model_or_default("claude-opus-4-6"));
        assert_eq!(opus, 9000, "a priced model must not report zero cost");

        // Haiku is cheaper than Opus, and both are real numbers.
        let haiku = cost_cents(usage, pricing_for_model_or_default("claude-haiku-4-5"));
        assert!(haiku > 0 && haiku < opus, "haiku={haiku} opus={opus}");

        // Unknown models fall back to the REPL's default tier rather than
        // panicking or reporting free.
        let unknown = cost_cents(usage, pricing_for_model_or_default("totally-made-up-model"));
        assert!(unknown > 0, "an unknown model must not report zero cost");
    }

    #[test]
    fn zero_usage_costs_nothing_and_does_not_panic() {
        assert_eq!(
            cost_cents(TokenUsage::default(), pricing_for_model_or_default("")),
            0
        );
    }

    #[test]
    fn usage_events_reach_the_status_bar_priced() {
        let (tx, rx) = mpsc::channel();
        let mut sink = ChannelSink::new(tx, "claude-opus-4-6");

        sink.usage(TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenUsage::default()
        })
        .expect("send");
        drop(sink);

        let events: Vec<StreamEvent> = rx.iter().collect();
        assert!(
            matches!(
                events.as_slice(),
                [StreamEvent::Usage {
                    input_tokens: 1_000_000,
                    cost_cents: 1500,
                    ..
                }]
            ),
            "usage must arrive priced, got {events:?}"
        );
    }

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
        let mut sink = ChannelSink::new(tx, MODEL);

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
        let mut sink = ChannelSink::new(tx, MODEL);

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
        let mut sink = ChannelSink::new(tx, MODEL);

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

    /// Drives a real turn against the configured provider through the same
    /// session worker the frontend runs. Opt-in because it needs credentials
    /// and network:
    ///   CLAW_TUI_LIVE_MODEL=<model> cargo test -p claw-tui -- --ignored
    #[test]
    #[ignore = "requires a configured provider and network"]
    fn live_turn_streams_assistant_text_from_the_configured_endpoint() {
        let Ok(model) = std::env::var("CLAW_TUI_LIVE_MODEL") else {
            panic!("set CLAW_TUI_LIVE_MODEL to the model to exercise");
        };
        let session =
            super::spawn_session(std::env::current_dir().expect("cwd"), mpsc::channel().0);

        assert!(
            session.run_turn(
                std::env::var("CLAW_TUI_LIVE_PROMPT")
                    .unwrap_or_else(|_| "reply with exactly: TUILIVE".to_string()),
                model,
                runtime::PermissionMode::Prompt,
            ),
            "the session worker should accept the turn"
        );

        let mut text = String::new();
        let mut failure = None;
        // The worker holds its sender for the whole session, so read until the
        // turn's own terminal event rather than until the channel closes.
        loop {
            match session
                .events()
                .recv_timeout(std::time::Duration::from_secs(120))
                .expect("the turn should finish")
            {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::Failed(error) => {
                    failure = Some(error);
                    break;
                }
                StreamEvent::Done { .. } => break,
                _ => {}
            }
        }

        assert!(failure.is_none(), "turn failed: {failure:?}");
        let expected =
            std::env::var("CLAW_TUI_LIVE_EXPECT").unwrap_or_else(|_| "TUILIVE".to_string());
        assert!(
            text.contains(&expected),
            "expected streamed assistant text, got {text:?}"
        );
        assert!(
            session
                .shutdown()
                .is_some_and(|session| !session.messages.is_empty()),
            "the session must come back with conversation history"
        );
    }

    #[test]
    fn an_approved_tool_unblocks_the_waiting_turn() {
        use runtime::{PermissionPrompter, PermissionRequest};

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
        use runtime::{PermissionPrompter, PermissionRequest};

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
        use runtime::{PermissionPrompter, PermissionRequest};

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
        let mut sink = ChannelSink::new(tx, MODEL);

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
        let mut sink = ChannelSink::new(tx, MODEL);
        drop(rx);

        // The frontend quit mid-turn; the engine should not see an error.
        sink.text_delta("orphaned").expect("send must be lossy");
        sink.turn_end().expect("send must be lossy");
    }
}

/// What the frontend resolves from `settings.json` before a turn.
///
/// The session runtime reads the user's configuration, so these cover the
/// difference between honouring it and ignoring it. They exercise the same
/// `claw_engine::setup` entry points the runtime factory calls, without needing
/// a provider or network.
#[cfg(test)]
mod config_tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use runtime::{ConfigLoader, PermissionMode, PermissionOutcome, PermissionPolicy};

    /// A scratch directory that removes itself, so a failing assertion cannot
    /// leave fixtures behind.
    pub(super) struct TempDir(PathBuf);

    impl TempDir {
        pub(super) fn new(label: &str) -> Self {
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

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Resolve configuration exactly as the runtime factory does, for an
    /// isolated workspace and config home.
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
    fn the_runtime_lifecycle_shuts_mcp_down_when_the_session_ends() {
        use super::RuntimeLifecycle;

        let workspace = TempDir::new("lifecycle-workspace");
        let config_home = TempDir::new("lifecycle-config");
        let state = plugin_state_for(
            workspace.path(),
            config_home.path(),
            r#"{"mcpServers":{"alpha":{"command":"claw-tui-no-such-mcp-binary","args":[]}}}"#,
        );
        let mcp_state = state.mcp_state.clone().expect("mcp state");

        // Dropping the guard is what the end of a session — or a rebuild —
        // does.
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

/// Persisting the frontend's session where the REPL keeps its own.
#[cfg(test)]
mod persistence_tests {
    use super::config_tests::TempDir;
    use super::{persistence_path_for, SessionPersistence};
    use runtime::{ConversationMessage, Session};
    use std::sync::mpsc;

    #[test]
    fn a_frontend_session_round_trips_through_the_repls_session_store() {
        let workspace = TempDir::new("persist-workspace");
        let (events, _rx) = mpsc::channel();

        let (mut session, mut persistence) = SessionPersistence::attach(
            Session::new().with_workspace_root(workspace.path()),
            workspace.path(),
            &events,
        );
        session
            .push_message(ConversationMessage::user_text("remember this"))
            .expect("append");
        persistence.save(&session, &events);

        let path = persistence.path.clone().expect("a session path");
        let loaded = Session::load_from_path(&path).expect("the session should load back");
        let texts: Vec<&str> = loaded
            .messages
            .iter()
            .flat_map(|message| &message.blocks)
            .filter_map(|block| match block {
                runtime::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["remember this"],
            "a session written by the frontend must load back with its messages"
        );
        assert_eq!(loaded.session_id, session.session_id);
    }

    #[test]
    fn the_session_lands_where_the_repl_looks_for_it() {
        // Same store and same extension as `rusty-claude-cli`, or `clawcli
        // --resume` cannot see frontend sessions.
        let workspace = TempDir::new("store-workspace");
        let session = Session::new();

        let path = persistence_path_for(workspace.path(), &session.session_id)
            .expect("a session path should resolve");

        let store = runtime::SessionStore::from_cwd(workspace.path()).expect("store");
        assert_eq!(
            path,
            store.create_handle(&session.session_id).path,
            "the frontend must write to the REPL's per-workspace session store"
        );
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("jsonl"));
    }

    #[test]
    fn a_new_session_is_written_before_its_first_turn() {
        // `--resume` should be able to see a session the user has not spoken
        // in yet.
        let workspace = TempDir::new("early-workspace");
        let (events, _rx) = mpsc::channel();

        let (_session, persistence) = SessionPersistence::attach(
            Session::new().with_workspace_root(workspace.path()),
            workspace.path(),
            &events,
        );

        assert!(persistence.path.as_ref().is_some_and(|path| path.exists()));
    }
}
