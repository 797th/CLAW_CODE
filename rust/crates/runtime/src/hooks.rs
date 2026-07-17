use std::ffi::OsStr;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{RuntimeFeatureConfig, RuntimeHookCommand, RuntimeHookConfig};
use crate::permissions::PermissionOverride;

const HOOK_PREVIEW_CHAR_LIMIT: usize = 160;

pub type HookPermissionDecision = PermissionOverride;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    Stop,
    PreCompact,
}

impl HookEvent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::Stop => "Stop",
            Self::PreCompact => "PreCompact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookProgressEvent {
    Started {
        event: HookEvent,
        tool_name: String,
        command: String,
    },
    Completed {
        event: HookEvent,
        tool_name: String,
        command: String,
    },
    Cancelled {
        event: HookEvent,
        tool_name: String,
        command: String,
    },
}

pub trait HookProgressReporter {
    fn on_event(&mut self, event: &HookProgressEvent);
}

#[derive(Debug, Clone, Default)]
pub struct HookAbortSignal {
    aborted: Arc<AtomicBool>,
}

impl HookAbortSignal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunResult {
    denied: bool,
    failed: bool,
    cancelled: bool,
    messages: Vec<String>,
    permission_override: Option<PermissionOverride>,
    permission_reason: Option<String>,
    updated_input: Option<String>,
}

impl HookRunResult {
    #[must_use]
    pub fn allow(messages: Vec<String>) -> Self {
        Self {
            denied: false,
            failed: false,
            cancelled: false,
            messages,
            permission_override: None,
            permission_reason: None,
            updated_input: None,
        }
    }

    #[must_use]
    pub fn is_denied(&self) -> bool {
        self.denied
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    #[must_use]
    pub fn permission_override(&self) -> Option<PermissionOverride> {
        self.permission_override
    }

    #[must_use]
    pub fn permission_decision(&self) -> Option<HookPermissionDecision> {
        self.permission_override
    }

    #[must_use]
    pub fn permission_reason(&self) -> Option<&str> {
        self.permission_reason.as_deref()
    }

    #[must_use]
    pub fn updated_input(&self) -> Option<&str> {
        self.updated_input.as_deref()
    }

    #[must_use]
    pub fn updated_input_json(&self) -> Option<&str> {
        self.updated_input()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookRunner {
    config: RuntimeHookConfig,
}

impl HookRunner {
    #[must_use]
    pub fn new(config: RuntimeHookConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn from_feature_config(feature_config: &RuntimeFeatureConfig) -> Self {
        Self::new(feature_config.hooks().clone())
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        self.run_pre_tool_use_with_context(tool_name, tool_input, None, None)
    }

    #[must_use]
    pub fn run_pre_tool_use_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PreToolUse,
            self.config.pre_tool_use_entries(),
            tool_name,
            tool_input,
            None,
            false,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_pre_tool_use_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_pre_tool_use_with_context(tool_name, tool_input, abort_signal, None)
    }

    #[must_use]
    pub fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
    ) -> HookRunResult {
        self.run_post_tool_use_with_context(
            tool_name,
            tool_input,
            tool_output,
            is_error,
            None,
            None,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PostToolUse,
            self.config.post_tool_use_entries(),
            tool_name,
            tool_input,
            Some(tool_output),
            is_error,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_post_tool_use_with_context(
            tool_name,
            tool_input,
            tool_output,
            is_error,
            abort_signal,
            None,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_failure(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
    ) -> HookRunResult {
        self.run_post_tool_use_failure_with_context(tool_name, tool_input, tool_error, None, None)
    }

    #[must_use]
    pub fn run_post_tool_use_failure_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PostToolUseFailure,
            self.config.post_tool_use_failure_entries(),
            tool_name,
            tool_input,
            Some(tool_error),
            true,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_failure_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_post_tool_use_failure_with_context(
            tool_name,
            tool_input,
            tool_error,
            abort_signal,
            None,
        )
    }

    /// Runs the hook commands configured for a lifecycle event
    /// (`SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`,
    /// `PreCompact`) through the JSON hook decision protocol
    /// (`run_hook_command`/`classify_hook_exit`), the same protocol
    /// implementation the tool-use path (`run_command`) uses. `payload`
    /// should carry whatever fields are applicable for `event` (e.g.
    /// `prompt`/`cwd` for `UserPromptSubmit`); `hook_event_name` is merged in
    /// automatically. Commands run in configured order; the first `Block`
    /// short-circuits the remaining commands. Tool-use events
    /// (`PreToolUse`/`PostToolUse`/`PostToolUseFailure`) have no configured
    /// commands here — use `run_pre_tool_use`/`run_post_tool_use`/
    /// `run_post_tool_use_failure` for those.
    #[must_use]
    pub fn run_lifecycle(&self, event: HookEvent, payload: &Value) -> HookOutcome {
        let commands: &[RuntimeHookCommand] = match event {
            HookEvent::SessionStart => self.config.session_start_entries(),
            HookEvent::SessionEnd => self.config.session_end_entries(),
            HookEvent::UserPromptSubmit => self.config.user_prompt_submit_entries(),
            HookEvent::Stop => self.config.stop_entries(),
            HookEvent::PreCompact => self.config.pre_compact_entries(),
            HookEvent::PreToolUse | HookEvent::PostToolUse | HookEvent::PostToolUseFailure => &[],
        };

        let mut contexts = Vec::new();
        let mut warning = None;

        for command in commands {
            match run_hook_command(command.command(), event, payload) {
                Ok(outcome) => {
                    if let Some(context) = outcome.additional_context {
                        contexts.push(context);
                    }
                    if warning.is_none() {
                        warning = outcome.warning;
                    }
                    if matches!(outcome.decision, HookDecision::Block { .. }) {
                        return HookOutcome {
                            decision: outcome.decision,
                            additional_context: join_hook_contexts(contexts),
                            warning,
                        };
                    }
                }
                Err(error) => {
                    if warning.is_none() {
                        warning = Some(format!(
                            "{} hook `{}` failed to start: {error}",
                            event.as_str(),
                            command.command()
                        ));
                    }
                }
            }
        }

        HookOutcome {
            decision: HookDecision::Allow,
            additional_context: join_hook_contexts(contexts),
            warning,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_commands(
        event: HookEvent,
        commands: &[RuntimeHookCommand],
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
        mut reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        if commands.is_empty() {
            return HookRunResult::allow(Vec::new());
        }

        if abort_signal.is_some_and(HookAbortSignal::is_aborted) {
            return HookRunResult {
                denied: false,
                failed: false,
                cancelled: true,
                messages: vec![format!(
                    "{} hook cancelled before execution",
                    event.as_str()
                )],
                permission_override: None,
                permission_reason: None,
                updated_input: None,
            };
        }

        let payload = hook_payload(event, tool_name, tool_input, tool_output, is_error).to_string();
        let mut result = HookRunResult::allow(Vec::new());

        for command in commands
            .iter()
            .filter(|command| command.matches_tool(tool_name))
        {
            let command_text = command.command();
            if let Some(reporter) = reporter.as_deref_mut() {
                reporter.on_event(&HookProgressEvent::Started {
                    event,
                    tool_name: tool_name.to_string(),
                    command: command_text.to_string(),
                });
            }

            match Self::run_command(
                command_text,
                event,
                tool_name,
                tool_input,
                tool_output,
                is_error,
                &payload,
                abort_signal,
            ) {
                HookCommandOutcome::Allow { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command_text.to_string(),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                }
                HookCommandOutcome::Deny { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command_text.to_string(),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                    result.denied = true;
                    return result;
                }
                HookCommandOutcome::Failed { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command_text.to_string(),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                    result.failed = true;
                    return result;
                }
                HookCommandOutcome::Cancelled { message } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Cancelled {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command_text.to_string(),
                        });
                    }
                    result.cancelled = true;
                    result.messages.push(message);
                    return result;
                }
            }
        }

        result
    }

    #[allow(clippy::too_many_arguments)]
    fn run_command(
        command: &str,
        event: HookEvent,
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
        payload: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookCommandOutcome {
        let mut child = shell_command(command);
        child.stdin(Stdio::piped());
        child.stdout(Stdio::piped());
        child.stderr(Stdio::piped());
        child.env("HOOK_EVENT", event.as_str());
        child.env("HOOK_TOOL_NAME", tool_name);
        child.env("HOOK_TOOL_INPUT", tool_input);
        child.env("HOOK_TOOL_IS_ERROR", if is_error { "1" } else { "0" });
        if let Some(tool_output) = tool_output {
            child.env("HOOK_TOOL_OUTPUT", tool_output);
        }

        match child.output_with_stdin(payload.as_bytes(), abort_signal) {
            Ok(CommandExecution::Finished(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let mut parsed = parse_hook_output(event, tool_name, command, &stdout, &stderr);

                // Run the same JSON hook decision protocol used by the
                // standalone `run_hook_command` entry point over this
                // command's captured stdout/stderr/exit code, and fold the
                // canonical decision into the legacy `ParsedHookOutput` so
                // both code paths agree on Allow/Block and PreToolUse denials
                // keep producing a permission override even when the hook
                // only speaks the plain `{"decision":"block",...}`/exit-2
                // protocol rather than the richer `hookSpecificOutput` shape.
                let outcome = classify_hook_exit(&stdout, &stderr, output.status.code());
                if let HookDecision::Block { .. } = &outcome.decision {
                    parsed.deny = true;
                    if event == HookEvent::PreToolUse && parsed.permission_override.is_none() {
                        parsed.permission_override = outcome.permission_override_for_pre_tool_use();
                    }
                }
                if let Some(context) = outcome.additional_context.as_deref() {
                    if !parsed.messages.iter().any(|message| message == context) {
                        parsed.messages.push(context.to_string());
                    }
                }

                let primary_message = parsed.primary_message().map(ToOwned::to_owned);
                match output.status.code() {
                    Some(0) => {
                        if parsed.deny {
                            HookCommandOutcome::Deny { parsed }
                        } else {
                            HookCommandOutcome::Allow { parsed }
                        }
                    }
                    Some(2) => HookCommandOutcome::Deny {
                        parsed: parsed.with_fallback_message(deny_fallback_message(
                            event, tool_name, &stderr,
                        )),
                    },
                    // Any other exit code (or a signal-terminated process,
                    // below) is a non-blocking failure *unless* the hook's
                    // stdout already carried an explicit JSON block decision
                    // (`parsed.deny`, set above from `outcome.decision`) — the
                    // JSON stdout decision wins over exit-code inference
                    // unconditionally, so a hook that emits
                    // `{"decision":"block",...}` but happens to exit 1 (or is
                    // killed) must still deny, not report a generic failure.
                    Some(code) => {
                        if parsed.deny {
                            HookCommandOutcome::Deny {
                                parsed: parsed.with_fallback_message(deny_fallback_message(
                                    event, tool_name, &stderr,
                                )),
                            }
                        } else {
                            HookCommandOutcome::Failed {
                                parsed: parsed.with_fallback_message(format_hook_failure(
                                    command,
                                    code,
                                    primary_message.as_deref(),
                                    stderr.as_str(),
                                )),
                            }
                        }
                    }
                    None => {
                        if parsed.deny {
                            HookCommandOutcome::Deny {
                                parsed: parsed.with_fallback_message(deny_fallback_message(
                                    event, tool_name, &stderr,
                                )),
                            }
                        } else {
                            HookCommandOutcome::Failed {
                                parsed: parsed.with_fallback_message(format!(
                                    "{} hook `{command}` terminated by signal while handling `{}`",
                                    event.as_str(),
                                    tool_name
                                )),
                            }
                        }
                    }
                }
            }
            Ok(CommandExecution::Cancelled) => HookCommandOutcome::Cancelled {
                message: format!(
                    "{} hook `{command}` cancelled while handling `{tool_name}`",
                    event.as_str()
                ),
            },
            Err(error) => HookCommandOutcome::Failed {
                parsed: ParsedHookOutput {
                    messages: vec![format!(
                        "{} hook `{command}` failed to start for `{}`: {error}",
                        event.as_str(),
                        tool_name
                    )],
                    ..ParsedHookOutput::default()
                },
            },
        }
    }
}

fn join_hook_contexts(contexts: Vec<String>) -> Option<String> {
    if contexts.is_empty() {
        None
    } else {
        Some(contexts.join("\n\n"))
    }
}

enum HookCommandOutcome {
    Allow { parsed: ParsedHookOutput },
    Deny { parsed: ParsedHookOutput },
    Failed { parsed: ParsedHookOutput },
    Cancelled { message: String },
}

/// Result of running a single hook command through the JSON stdin/stdout
/// decision protocol (see `run_hook_command`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookOutcome {
    pub decision: HookDecision,
    pub additional_context: Option<String>,
    /// Set when the hook process exited with a code other than 0/2, or was
    /// terminated by a signal: a non-blocking execution problem that still
    /// resolves to `Allow` but that callers should surface/log rather than
    /// silently ignore (mirrors `HookRunner`'s tool-use `Failed` outcome).
    pub warning: Option<String>,
}

impl HookOutcome {
    /// Maps this outcome onto the legacy `PreToolUse` permission override so
    /// callers that still consume `HookPermissionDecision` keep working: a
    /// `Block` decision denies the tool call, `Allow` leaves the existing
    /// permission pipeline untouched.
    #[must_use]
    pub fn permission_override_for_pre_tool_use(&self) -> Option<PermissionOverride> {
        match &self.decision {
            HookDecision::Block { .. } => Some(PermissionOverride::Deny),
            HookDecision::Allow => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HookDecision {
    #[default]
    Allow,
    Block {
        reason: String,
    },
}

/// Runs a single hook `command` for `event`, sending `payload` (with
/// `hook_event_name` merged in) as JSON on the child's stdin and mapping the
/// result to a `HookOutcome` per the JSON hook decision protocol. This is the
/// same decision logic (`classify_hook_exit`) that `HookRunner`'s tool-use
/// path (`run_command`) uses internally, so lifecycle callers (see
/// `HookRunner::run_lifecycle`) and tool-use hooks share one implementation
/// of the protocol:
///
/// - exit 0 -> Allow
/// - exit 2 -> Block, reason = stderr
/// - JSON stdout with `{"decision":"block","reason":...}` overrides the
///   exit-code inference
/// - `additionalContext` in JSON stdout (or, for non-JSON stdout, the raw
///   stdout text) is carried as `HookOutcome::additional_context`
/// - any other exit code, or a signal-terminated process, is treated as a
///   non-blocking failure: `decision` stays `Allow` but `HookOutcome::warning`
///   is populated so callers can surface/log it (mirrors the `Failed`
///   handling `HookRunner`'s tool-use path already has for these exit codes).
pub fn run_hook_command(
    command: &str,
    event: HookEvent,
    payload: &Value,
) -> std::io::Result<HookOutcome> {
    let mut full_payload = payload.clone();
    if let Value::Object(map) = &mut full_payload {
        map.insert(
            "hook_event_name".to_string(),
            Value::String(event.as_str().to_string()),
        );
    }
    let payload_bytes = serde_json::to_vec(&full_payload)?;

    let mut child = shell_command(command);
    child.stdin(Stdio::piped());
    child.stdout(Stdio::piped());
    child.stderr(Stdio::piped());

    match child.output_with_stdin(&payload_bytes, None)? {
        CommandExecution::Finished(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Ok(classify_hook_exit(&stdout, &stderr, output.status.code()))
        }
        CommandExecution::Cancelled => Ok(HookOutcome::default()),
    }
}

#[cfg(test)]
pub(crate) fn run_hook_command_for_test(
    command: &str,
    event: HookEvent,
    payload: &Value,
) -> HookOutcome {
    run_hook_command(command, event, payload).expect("hook command should run")
}

/// The single implementation of the JSON hook decision protocol: given the
/// captured stdout/stderr and exit code of a hook process, decides
/// Allow/Block (+ reason), additional context, and any non-blocking-failure
/// warning. Shared by `run_hook_command` (used by `HookRunner::run_lifecycle`
/// and directly by callers of the standalone protocol) and by
/// `HookRunner::run_command` (the tool-use path), so there is exactly one
/// place that interprets a hook's stdout/stderr/exit code.
fn classify_hook_exit(stdout: &str, stderr: &str, code: Option<i32>) -> HookOutcome {
    if let Ok(Value::Object(root)) = serde_json::from_str::<Value>(stdout) {
        let additional_context = root
            .get("additionalContext")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        if root.get("decision").and_then(Value::as_str) == Some("block") {
            let reason = root
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            return HookOutcome {
                decision: HookDecision::Block { reason },
                additional_context,
                warning: None,
            };
        }

        let (decision, warning) = exit_code_decision(code, stderr);
        return HookOutcome {
            decision,
            additional_context,
            warning,
        };
    }

    let (decision, warning) = exit_code_decision(code, stderr);
    let additional_context =
        if matches!(decision, HookDecision::Allow) && warning.is_none() && !stdout.is_empty() {
            Some(stdout.to_string())
        } else {
            None
        };
    HookOutcome {
        decision,
        additional_context,
        warning,
    }
}

/// Maps a hook process's exit code to a decision plus an optional
/// non-blocking-failure warning. Exit 0 allows; exit 2 blocks with `stderr`
/// as the reason; any other exit code, or a signal-terminated process
/// (`code` is `None`), is a non-blocking failure: it still allows, but a
/// warning is returned so the caller can surface/log it rather than silently
/// swallowing the unexpected exit.
fn exit_code_decision(code: Option<i32>, stderr: &str) -> (HookDecision, Option<String>) {
    match code {
        Some(0) => (HookDecision::Allow, None),
        Some(2) => (
            HookDecision::Block {
                reason: stderr.to_string(),
            },
            None,
        ),
        Some(other) => {
            let warning = if stderr.is_empty() {
                format!("hook exited with non-blocking status {other}")
            } else {
                format!("hook exited with non-blocking status {other}: {stderr}")
            };
            (HookDecision::Allow, Some(warning))
        }
        None => (
            HookDecision::Allow,
            Some("hook terminated by signal".to_string()),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ParsedHookOutput {
    messages: Vec<String>,
    deny: bool,
    permission_override: Option<PermissionOverride>,
    permission_reason: Option<String>,
    updated_input: Option<String>,
}

impl ParsedHookOutput {
    fn with_fallback_message(mut self, fallback: String) -> Self {
        if self.messages.is_empty() {
            self.messages.push(fallback);
        }
        self
    }

    fn primary_message(&self) -> Option<&str> {
        self.messages.first().map(String::as_str)
    }
}

fn merge_parsed_hook_output(target: &mut HookRunResult, parsed: ParsedHookOutput) {
    target.messages.extend(parsed.messages);
    if parsed.permission_override.is_some() {
        target.permission_override = parsed.permission_override;
    }
    if parsed.permission_reason.is_some() {
        target.permission_reason = parsed.permission_reason;
    }
    if parsed.updated_input.is_some() {
        target.updated_input = parsed.updated_input;
    }
}

fn parse_hook_output(
    event: HookEvent,
    tool_name: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
) -> ParsedHookOutput {
    if stdout.is_empty() {
        return ParsedHookOutput::default();
    }

    let root = match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(root)) => root,
        Ok(value) => {
            return ParsedHookOutput {
                messages: vec![format_invalid_hook_output(
                    event,
                    tool_name,
                    command,
                    &format!(
                        "expected top-level JSON object, got {}",
                        json_type_name(&value)
                    ),
                    stdout,
                    stderr,
                )],
                ..ParsedHookOutput::default()
            };
        }
        Err(error) if looks_like_json_attempt(stdout) => {
            return ParsedHookOutput {
                messages: vec![format_invalid_hook_output(
                    event,
                    tool_name,
                    command,
                    &error.to_string(),
                    stdout,
                    stderr,
                )],
                ..ParsedHookOutput::default()
            };
        }
        Err(_) => {
            return ParsedHookOutput {
                messages: vec![stdout.to_string()],
                ..ParsedHookOutput::default()
            };
        }
    };

    let mut parsed = ParsedHookOutput::default();

    if let Some(message) = root.get("systemMessage").and_then(Value::as_str) {
        parsed.messages.push(message.to_string());
    }
    if let Some(message) = root.get("reason").and_then(Value::as_str) {
        parsed.messages.push(message.to_string());
    }
    if root.get("continue").and_then(Value::as_bool) == Some(false)
        || root.get("decision").and_then(Value::as_str) == Some("block")
    {
        parsed.deny = true;
    }

    if let Some(Value::Object(specific)) = root.get("hookSpecificOutput") {
        if let Some(Value::String(additional_context)) = specific.get("additionalContext") {
            parsed.messages.push(additional_context.clone());
        }
        if let Some(decision) = specific.get("permissionDecision").and_then(Value::as_str) {
            parsed.permission_override = match decision {
                "allow" => Some(PermissionOverride::Allow),
                "deny" => Some(PermissionOverride::Deny),
                "ask" => Some(PermissionOverride::Ask),
                _ => None,
            };
        }
        if let Some(reason) = specific
            .get("permissionDecisionReason")
            .and_then(Value::as_str)
        {
            parsed.permission_reason = Some(reason.to_string());
        }
        if let Some(updated_input) = specific.get("updatedInput") {
            parsed.updated_input = serde_json::to_string(updated_input).ok();
        }
    }

    if parsed.messages.is_empty() {
        parsed.messages.push(stdout.to_string());
    }

    parsed
}

fn hook_payload(
    event: HookEvent,
    tool_name: &str,
    tool_input: &str,
    tool_output: Option<&str>,
    is_error: bool,
) -> Value {
    match event {
        HookEvent::PostToolUseFailure => json!({
            "hook_event_name": event.as_str(),
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_error": tool_output,
            "tool_result_is_error": true,
        }),
        _ => json!({
            "hook_event_name": event.as_str(),
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_output": tool_output,
            "tool_result_is_error": is_error,
        }),
    }
}

fn parse_tool_input(tool_input: &str) -> Value {
    serde_json::from_str(tool_input).unwrap_or_else(|_| json!({ "raw": tool_input }))
}

fn format_invalid_hook_output(
    event: HookEvent,
    tool_name: &str,
    command: &str,
    detail: &str,
    stdout: &str,
    stderr: &str,
) -> String {
    let stdout_preview = bounded_hook_preview(stdout).unwrap_or_else(|| "<empty>".to_string());
    let stderr_preview = bounded_hook_preview(stderr).unwrap_or_else(|| "<empty>".to_string());
    let command_preview = bounded_hook_preview(command).unwrap_or_else(|| "<empty>".to_string());

    format!(
        "hook_invalid_json: phase={} tool={} command={} detail={} stdout_preview={} stderr_preview={}",
        event.as_str(),
        tool_name,
        command_preview,
        detail,
        stdout_preview,
        stderr_preview
    )
}

fn bounded_hook_preview(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut preview = String::new();
    for (count, ch) in trimmed.chars().enumerate() {
        if count == HOOK_PREVIEW_CHAR_LIMIT {
            preview.push('…');
            break;
        }
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            '\t' => preview.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(&mut preview, "\\u{{{:x}}}", control as u32);
            }
            _ => preview.push(ch),
        }
    }
    Some(preview)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn looks_like_json_attempt(value: &str) -> bool {
    matches!(value.trim_start().chars().next(), Some('{' | '['))
}

/// Fallback deny reason used when a hook's own messages don't already supply
/// one (`ParsedHookOutput::with_fallback_message` only applies it if
/// `messages` is empty): prefers `stderr`, matching the exit-2 contract, and
/// otherwise falls back to a generic denial message.
fn deny_fallback_message(event: HookEvent, tool_name: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        format!("{} hook denied tool `{tool_name}`", event.as_str())
    } else {
        stderr.to_string()
    }
}

fn format_hook_failure(command: &str, code: i32, stdout: Option<&str>, stderr: &str) -> String {
    let mut message = format!("Hook `{command}` exited with status {code}");
    if let Some(stdout) = stdout.filter(|stdout| !stdout.is_empty()) {
        message.push_str(": ");
        message.push_str(stdout);
    } else if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }
    message
}

fn shell_command(command: &str) -> CommandWithStdin {
    #[cfg(windows)]
    let command_builder = {
        let mut command_builder = Command::new("cmd");
        command_builder.arg("/C").arg(command);
        CommandWithStdin::new(command_builder)
    };

    #[cfg(not(windows))]
    let command_builder = {
        let mut command_builder = Command::new("sh");
        command_builder.arg("-lc").arg(command);
        CommandWithStdin::new(command_builder)
    };

    command_builder
}

struct CommandWithStdin {
    command: Command,
}

impl CommandWithStdin {
    fn new(command: Command) -> Self {
        Self { command }
    }

    fn stdin(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stdin(cfg);
        self
    }

    fn stdout(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stdout(cfg);
        self
    }

    fn stderr(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stderr(cfg);
        self
    }

    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    fn output_with_stdin(
        &mut self,
        stdin: &[u8],
        abort_signal: Option<&HookAbortSignal>,
    ) -> std::io::Result<CommandExecution> {
        let mut child = self.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(stdin)?;
        }

        loop {
            if abort_signal.is_some_and(HookAbortSignal::is_aborted) {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Ok(CommandExecution::Cancelled);
            }

            match child.try_wait()? {
                Some(_) => return child.wait_with_output().map(CommandExecution::Finished),
                None => thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

enum CommandExecution {
    Finished(std::process::Output),
    Cancelled,
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use serde_json::json;

    use super::{
        run_hook_command_for_test, HookAbortSignal, HookDecision, HookEvent, HookOutcome,
        HookProgressEvent, HookProgressReporter, HookRunResult, HookRunner,
    };
    use crate::config::{RuntimeFeatureConfig, RuntimeHookCommand, RuntimeHookConfig};
    use crate::permissions::PermissionOverride;

    struct RecordingReporter {
        events: Vec<HookProgressEvent>,
    }

    impl HookProgressReporter for RecordingReporter {
        fn on_event(&mut self, event: &HookProgressEvent) {
            self.events.push(event.clone());
        }
    }

    #[test]
    fn lifecycle_hook_events_have_canonical_names() {
        assert_eq!(HookEvent::SessionStart.as_str(), "SessionStart");
        assert_eq!(HookEvent::SessionEnd.as_str(), "SessionEnd");
        assert_eq!(HookEvent::UserPromptSubmit.as_str(), "UserPromptSubmit");
        assert_eq!(HookEvent::Stop.as_str(), "Stop");
        assert_eq!(HookEvent::PreCompact.as_str(), "PreCompact");
    }

    #[test]
    fn allows_exit_code_zero_and_captures_stdout() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'pre ok'")],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert_eq!(result, HookRunResult::allow(vec!["pre ok".to_string()]));
    }

    #[test]
    fn object_style_hook_matchers_filter_runtime_execution() {
        let runner = HookRunner::new(RuntimeHookConfig::from_hook_commands(
            vec![
                RuntimeHookCommand::new(shell_snippet("printf 'legacy'")),
                RuntimeHookCommand::with_matcher(
                    shell_snippet("printf 'bash only'"),
                    Some("Bash".to_string()),
                ),
                RuntimeHookCommand::with_matcher(
                    shell_snippet("printf 'read only'"),
                    Some("Read*".to_string()),
                ),
            ],
            Vec::new(),
            Vec::new(),
        ));

        let read_result = runner.run_pre_tool_use("ReadFile", r#"{"path":"README.md"}"#);
        let bash_result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert_eq!(
            read_result,
            HookRunResult::allow(vec!["legacy".to_string(), "read only".to_string()])
        );
        assert_eq!(
            bash_result,
            HookRunResult::allow(vec!["legacy".to_string(), "bash only".to_string()])
        );
    }

    #[test]
    fn denies_exit_code_two() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'blocked by hook'; exit 2")],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert!(result.is_denied());
        assert_eq!(result.messages(), &["blocked by hook".to_string()]);
    }

    #[test]
    fn json_block_decision_wins_over_non_blocking_exit_code_in_real_runner() {
        // Exercises HookRunner::run_command (the real PreToolUse execution
        // path, not the standalone run_hook_command), covering the case where
        // a hook exits 1 (normally a non-blocking `Failed`) but its stdout
        // carries an explicit JSON block decision. Per the protocol contract,
        // JSON stdout wins over exit-code inference unconditionally.
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(
                r#"printf '%s' '{"decision":"block","reason":"custom"}'; exit 1"#,
            )],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert!(result.is_denied());
        assert!(!result.is_failed());
        assert_eq!(result.permission_override(), Some(PermissionOverride::Deny));
        assert!(result.messages().iter().any(|message| message == "custom"));
    }

    #[test]
    fn propagates_other_non_zero_statuses_as_failures() {
        let runner = HookRunner::from_feature_config(&RuntimeFeatureConfig::default().with_hooks(
            RuntimeHookConfig::new(
                vec![shell_snippet("printf 'warning hook'; exit 1")],
                Vec::new(),
                Vec::new(),
            ),
        ));

        // given
        // when
        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("warning hook")));
    }

    #[test]
    fn parses_pre_hook_permission_override_and_updated_input() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(
                r#"printf '%s' '{"systemMessage":"updated","hookSpecificOutput":{"permissionDecision":"allow","permissionDecisionReason":"hook ok","updatedInput":{"command":"git status"}}}'"#,
            )],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("bash", r#"{"command":"pwd"}"#);

        assert_eq!(
            result.permission_override(),
            Some(PermissionOverride::Allow)
        );
        assert_eq!(result.permission_reason(), Some("hook ok"));
        assert_eq!(result.updated_input(), Some(r#"{"command":"git status"}"#));
        assert!(result.messages().iter().any(|message| message == "updated"));
    }

    #[test]
    fn runs_post_tool_use_failure_hooks() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            Vec::new(),
            Vec::new(),
            vec![shell_snippet("printf 'failure hook ran'")],
        ));

        // when
        let result =
            runner.run_post_tool_use_failure("bash", r#"{"command":"false"}"#, "command failed");

        // then
        assert!(!result.is_denied());
        assert_eq!(result.messages(), &["failure hook ran".to_string()]);
    }

    #[test]
    fn stops_running_failure_hooks_after_failure() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            Vec::new(),
            Vec::new(),
            vec![
                shell_snippet("printf 'broken failure hook'; exit 1"),
                shell_snippet("printf 'later failure hook'"),
            ],
        ));

        // when
        let result =
            runner.run_post_tool_use_failure("bash", r#"{"command":"false"}"#, "command failed");

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("broken failure hook")));
        assert!(!result
            .messages()
            .iter()
            .any(|message| message == "later failure hook"));
    }

    #[test]
    fn executes_hooks_in_configured_order() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![
                shell_snippet("printf 'first'"),
                shell_snippet("printf 'second'"),
            ],
            Vec::new(),
            Vec::new(),
        ));
        let mut reporter = RecordingReporter { events: Vec::new() };

        // when
        let result = runner.run_pre_tool_use_with_context(
            "Read",
            r#"{"path":"README.md"}"#,
            None,
            Some(&mut reporter),
        );

        // then
        assert_eq!(
            result,
            HookRunResult::allow(vec!["first".to_string(), "second".to_string()])
        );
        assert_eq!(reporter.events.len(), 4);
        assert!(matches!(
            &reporter.events[0],
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'first'"
        ));
        assert!(matches!(
            &reporter.events[1],
            HookProgressEvent::Completed {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'first'"
        ));
        assert!(matches!(
            &reporter.events[2],
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'second'"
        ));
        assert!(matches!(
            &reporter.events[3],
            HookProgressEvent::Completed {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'second'"
        ));
    }

    #[test]
    fn stops_running_hooks_after_failure() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![
                shell_snippet("printf 'broken'; exit 1"),
                shell_snippet("printf 'later'"),
            ],
            Vec::new(),
            Vec::new(),
        ));

        // when
        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("broken")));
        assert!(!result.messages().iter().any(|message| message == "later"));
    }

    #[test]
    fn malformed_nonempty_hook_output_reports_explicit_diagnostic_with_previews() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(
                "printf '{not-json\nsecond line'; printf 'stderr warning' >&2; exit 1",
            )],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        assert!(result.is_failed());
        let rendered = result.messages().join("\n");
        assert!(rendered.contains("hook_invalid_json:"));
        assert!(rendered.contains("phase=PreToolUse"));
        assert!(rendered.contains("tool=Edit"));
        assert!(rendered.contains("command=printf '{not-json"));
        assert!(rendered.contains("printf 'stderr warning' >&2; exit 1"));
        assert!(rendered.contains("detail=key must be a string"));
        assert!(rendered.contains("stdout_preview={not-json"));
        assert!(rendered.contains("second line stderr_preview=stderr warning"));
        assert!(rendered.contains("stderr_preview=stderr warning"));
    }

    #[test]
    fn abort_signal_cancels_long_running_hook_and_reports_progress() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("sleep 5")],
            Vec::new(),
            Vec::new(),
        ));
        let abort_signal = HookAbortSignal::new();
        let abort_signal_for_thread = abort_signal.clone();
        let mut reporter = RecordingReporter { events: Vec::new() };

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            abort_signal_for_thread.abort();
        });

        let result = runner.run_pre_tool_use_with_context(
            "bash",
            r#"{"command":"sleep 5"}"#,
            Some(&abort_signal),
            Some(&mut reporter),
        );

        assert!(result.is_cancelled());
        assert!(reporter.events.iter().any(|event| matches!(
            event,
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                ..
            }
        )));
        assert!(reporter.events.iter().any(|event| matches!(
            event,
            HookProgressEvent::Cancelled {
                event: HookEvent::PreToolUse,
                ..
            }
        )));
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }

    #[cfg(unix)]
    #[test]
    fn exit_two_blocks_with_stderr_reason() {
        let out = run_hook_command_for_test(
            "sh -c 'echo nope >&2; exit 2'",
            HookEvent::PreToolUse,
            &json!({"tool_name":"Bash"}),
        );
        assert!(
            matches!(out.decision, HookDecision::Block { ref reason } if reason.contains("nope"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn stdout_text_on_session_start_becomes_context() {
        let out = run_hook_command_for_test(
            "echo 'workflow: run /start-work first'",
            HookEvent::SessionStart,
            &json!({}),
        );
        assert_eq!(out.decision, HookDecision::Allow);
        assert!(out.additional_context.unwrap().contains("/start-work"));
    }

    #[cfg(unix)]
    #[test]
    fn json_stdout_decision_overrides_exit_code() {
        let out = run_hook_command_for_test(
            r#"echo '{"decision":"block","reason":"missing AC"}'"#,
            HookEvent::Stop,
            &json!({}),
        );
        assert!(matches!(out.decision, HookDecision::Block { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn json_stdout_reason_is_captured() {
        let out = run_hook_command_for_test(
            r#"echo '{"decision":"block","reason":"missing AC"}'"#,
            HookEvent::Stop,
            &json!({}),
        );
        assert!(
            matches!(out.decision, HookDecision::Block { ref reason } if reason == "missing AC")
        );
    }

    #[cfg(unix)]
    #[test]
    fn json_stdout_additional_context_without_decision_still_allows() {
        let out = run_hook_command_for_test(
            r#"echo '{"additionalContext":"remember to run tests"}'"#,
            HookEvent::UserPromptSubmit,
            &json!({"prompt": "hi"}),
        );
        assert_eq!(out.decision, HookDecision::Allow);
        assert_eq!(
            out.additional_context.as_deref(),
            Some("remember to run tests")
        );
    }

    #[cfg(unix)]
    #[test]
    fn exit_zero_empty_stdout_allows_with_no_context() {
        let out = run_hook_command_for_test("true", HookEvent::PreCompact, &json!({}));
        assert_eq!(
            out,
            HookOutcome {
                decision: HookDecision::Allow,
                additional_context: None,
                warning: None,
            }
        );
    }

    #[test]
    fn block_decision_maps_to_deny_permission_override_for_pre_tool_use() {
        let outcome = HookOutcome {
            decision: HookDecision::Block {
                reason: "no".to_string(),
            },
            additional_context: None,
            warning: None,
        };
        assert_eq!(
            outcome.permission_override_for_pre_tool_use(),
            Some(PermissionOverride::Deny)
        );
    }

    #[test]
    fn allow_decision_has_no_permission_override() {
        let outcome = HookOutcome::default();
        assert_eq!(outcome.permission_override_for_pre_tool_use(), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_blocking_exit_code_allows_but_surfaces_warning() {
        let out = run_hook_command_for_test(
            "sh -c 'echo trouble >&2; exit 1'",
            HookEvent::PreToolUse,
            &json!({"tool_name":"Bash"}),
        );
        assert_eq!(out.decision, HookDecision::Allow);
        let warning = out.warning.expect("exit code 1 should surface a warning");
        assert!(warning.contains('1'));
        assert!(warning.contains("trouble"));
    }

    #[cfg(unix)]
    #[test]
    fn signal_killed_hook_allows_but_surfaces_warning() {
        let out = run_hook_command_for_test(
            "sh -c 'kill -TERM $$'",
            HookEvent::PreToolUse,
            &json!({"tool_name":"Bash"}),
        );
        assert_eq!(out.decision, HookDecision::Allow);
        let warning = out
            .warning
            .expect("signal-terminated hook should surface a warning");
        assert!(warning.contains("signal"));
    }

    #[cfg(unix)]
    #[test]
    fn run_lifecycle_runs_configured_session_start_commands_through_json_protocol() {
        let config = RuntimeHookConfig::default().with_lifecycle_hook_commands(
            vec![RuntimeHookCommand::new(shell_snippet(
                "echo 'workflow: run /start-work first'",
            ))],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let runner = HookRunner::new(config);

        let outcome = runner.run_lifecycle(HookEvent::SessionStart, &json!({}));

        assert_eq!(outcome.decision, HookDecision::Allow);
        assert!(outcome.additional_context.unwrap().contains("/start-work"));
    }

    #[cfg(unix)]
    #[test]
    fn run_lifecycle_stop_event_blocks_on_json_decision() {
        let config = RuntimeHookConfig::default().with_lifecycle_hook_commands(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![RuntimeHookCommand::new(
                r#"echo '{"decision":"block","reason":"missing AC"}'"#.to_string(),
            )],
            Vec::new(),
        );
        let runner = HookRunner::new(config);

        let outcome = runner.run_lifecycle(HookEvent::Stop, &json!({}));

        assert!(
            matches!(outcome.decision, HookDecision::Block { ref reason } if reason == "missing AC")
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_lifecycle_is_a_noop_for_unconfigured_tool_use_events() {
        let runner = HookRunner::new(RuntimeHookConfig::default());
        let outcome = runner.run_lifecycle(HookEvent::PreToolUse, &json!({}));
        assert_eq!(outcome, HookOutcome::default());
    }
}
