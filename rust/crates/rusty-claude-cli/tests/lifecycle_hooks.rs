//! Task 3: fires `SessionStart`/`UserPromptSubmit`/`Stop`/`SessionEnd` lifecycle
//! hooks from the CLI's one-shot (`--output-format json`) agent loop.
//!
//! Follows the same real-binary + mock-Anthropic-service harness shape as
//! `mock_parity_harness.rs`'s `run_case`: the full `clawcli` binary is
//! launched against `mock_anthropic_service::MockAnthropicService` so the
//! turn actually completes end-to-end, rather than invoking any internal
//! runtime function directly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mock_anthropic_service::{MockAnthropicService, SCENARIO_PREFIX};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `SessionStart` fires once at session boot; its `additionalContext` is
/// appended to the first turn, so the hook's stdout text ends up persisted in
/// the session transcript alongside the original prompt. A marker file lets
/// the test assert the hook actually ran (not just that the turn succeeded).
#[test]
fn session_start_hook_runs_and_its_context_reaches_the_transcript() {
    let workspace = unique_temp_dir("session-start-hook");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    fs::write(
        workspace.join(".claw.json"),
        r#"{"hooks":{"SessionStart":["touch hook_ran.marker && printf 'session-start-hook-context'"]}}"#,
    )
    .expect("hook config should write");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let output = run_claw_one_shot(&workspace, &config_home, &home, &base_url);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The hook actually ran (not silently skipped/misconfigured).
    assert!(
        workspace.join("hook_ran.marker").exists(),
        "SessionStart hook should have created the marker file; stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The hook's stdout (its additionalContext) was appended to the first
    // turn and therefore persisted in the session transcript.
    let transcript = read_only_session_transcript(&workspace);
    assert!(
        transcript.contains("session-start-hook-context"),
        "SessionStart hook's additionalContext should be appended to the first turn's \
         transcript; transcript was:\n{transcript}"
    );
}

/// `UserPromptSubmit` returning a JSON block decision must cancel the turn
/// before any model call happens: the CLI should exit non-zero and print the
/// block reason, and the mock service must never see a request.
#[test]
fn user_prompt_submit_block_cancels_the_turn_before_any_model_call() {
    let workspace = unique_temp_dir("user-prompt-submit-block");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    fs::write(
        workspace.join(".claw.json"),
        r#"{"hooks":{"UserPromptSubmit":["echo '{\"decision\":\"block\",\"reason\":\"not now\"}'"]}}"#,
    )
    .expect("hook config should write");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let output = run_claw_one_shot(&workspace, &config_home, &home, &base_url);

    assert!(
        !output.status.success(),
        "a blocked UserPromptSubmit hook should cause the CLI to exit non-zero"
    );
    // `--output-format=json` routes error envelopes to stdout (#819/#820/#823)
    // so machine consumers can parse failures from stdout byte 0.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not now"),
        "the block reason should be printed in the JSON error envelope; stdout was:\n{stdout}"
    );

    let captured = runtime.block_on(server.captured_requests());
    assert!(
        captured.is_empty(),
        "a blocked UserPromptSubmit must cancel the turn before any model request is sent, \
         but the mock service captured: {captured:?}"
    );
}

/// `SessionEnd` fires fire-and-forget from `Drop for LiveCli` (main.rs), so
/// it should run on the normal one-shot exit path even though nothing in the
/// turn loop itself calls it directly.
#[test]
fn session_end_hook_runs_on_process_exit() {
    let workspace = unique_temp_dir("session-end-hook");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    fs::write(
        workspace.join(".claw.json"),
        r#"{"hooks":{"SessionEnd":["touch session_end_ran.marker"]}}"#,
    )
    .expect("hook config should write");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let output = run_claw_one_shot(&workspace, &config_home, &home, &base_url);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        workspace.join("session_end_ran.marker").exists(),
        "SessionEnd hook should have run (fire-and-forget) as the process exited; \
         stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_claw_one_shot(workspace: &Path, config_home: &Path, home: &Path, base_url: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clawcli"));
    command
        .current_dir(workspace)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-lifecycle-hooks-key")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .args([
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "--output-format=json",
            &format!("{SCENARIO_PREFIX}streaming_text"),
        ]);
    command.output().expect("clawcli should launch")
}

/// Reads the sole session transcript file written under
/// `<workspace>/.claw/sessions/<workspace-fingerprint>/` (`SessionStore::from_cwd`):
/// one-shot mode creates exactly one session per invocation.
fn read_only_session_transcript(workspace: &Path) -> String {
    let sessions_root = workspace.join(".claw").join("sessions");
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_jsonl_files(&sessions_root, &mut entries);
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one session transcript under {sessions_root:?}, found {entries:?}"
    );
    fs::read_to_string(entries.remove(0)).expect("session transcript should be readable")
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-lifecycle-hooks-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
