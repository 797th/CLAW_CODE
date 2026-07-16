//! Task 12: `clawcli init --harness` scaffolds a SAW-lite starter pack
//! (`.claw/agents/qas.md`, three `.claw/commands/*.md`, two
//! `.claw/skills/*/SKILL.md`, and a merged `session_start` hook in
//! `.claw.json`).
//!
//! Runs the real `clawcli` binary (there is no lib target on this crate, so
//! integration tests exercise it the same way the other tests here do —
//! e.g. `cli_flags_and_config_defaults.rs`, `lifecycle_hooks.rs`) and then
//! asserts on the resulting files using the *actual* discovery/parsing code
//! from the `runtime` and `commands` crates, not just raw string checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use commands::SlashCommand;
use runtime::harness_assets::{discover, AgentRole};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-init-harness-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}

fn run_init_harness(cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clawcli"))
        .current_dir(cwd)
        .args(["init", "--harness"])
        .output()
        .expect("clawcli init --harness should launch")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "clawcli init --harness should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const EXPECTED_FILES: [&str; 6] = [
    ".claw/agents/qas.md",
    ".claw/commands/start-work.md",
    ".claw/commands/pre-pr.md",
    ".claw/commands/end-work.md",
    ".claw/skills/pattern-discovery/SKILL.md",
    ".claw/skills/verification-before-completion/SKILL.md",
];

/// First run: creates the full starter-pack tree, and every asset parses
/// with the real discovery/parsing code the runtime actually uses.
#[test]
fn init_harness_creates_full_tree_and_assets_parse() {
    let workspace = unique_temp_dir("full-tree");
    fs::create_dir_all(&workspace).expect("workspace should exist");

    let output = run_init_harness(&workspace);
    assert_success(&output);

    for relative in EXPECTED_FILES {
        assert!(
            workspace.join(relative).is_file(),
            "expected {relative} to exist after init --harness"
        );
    }

    // .claw.json should exist and have a merged session_start hook.
    let claw_json_path = workspace.join(".claw.json");
    assert!(claw_json_path.is_file(), ".claw.json should be created");
    let claw_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claw_json_path).expect("read .claw.json"))
            .expect(".claw.json should be valid JSON");
    let session_start = claw_json
        .get("hooks")
        .and_then(|hooks| hooks.get("SessionStart"))
        .and_then(|value| value.as_array())
        .expect("hooks.SessionStart should be an array");
    assert_eq!(session_start.len(), 1);
    assert!(session_start[0]
        .as_str()
        .expect("session_start entry should be a string")
        .contains("/start-work"));

    // discover() finds all three asset kinds.
    let assets = discover(&workspace);
    assert!(
        assets.warnings.is_empty(),
        "discovery should not warn on scaffolded assets: {:?}",
        assets.warnings
    );

    let qas = assets
        .agents
        .iter()
        .find(|agent| agent.name == "qas")
        .expect("qas agent should be discovered");
    assert_eq!(
        qas.role,
        AgentRole::Gate,
        "qas.md should parse as role: gate"
    );

    for name in ["start-work", "pre-pr", "end-work"] {
        assert!(
            assets.commands.iter().any(|command| command.name == name),
            "command {name} should be discovered"
        );
    }
    assert!(
        assets
            .skills
            .iter()
            .any(|skill| skill.name == "pattern-discovery"),
        "pattern-discovery skill should be discovered"
    );
    assert!(
        assets
            .skills
            .iter()
            .any(|skill| skill.name == "verification-before-completion"),
        "verification-before-completion skill should be discovered"
    );

    // The two commands the brief calls out parse as Custom via
    // parse_with_commands, with $ARGUMENTS substituted.
    let parsed = SlashCommand::parse_with_commands("/start-work TASK-42", &assets.commands)
        .expect("start-work should parse")
        .expect("start-work should resolve to a command");
    match parsed {
        SlashCommand::Custom { name, body } => {
            assert_eq!(name, "start-work");
            assert!(
                body.contains("TASK-42"),
                "start-work body should have $ARGUMENTS substituted: {body}"
            );
            assert!(!body.contains("$ARGUMENTS"));
        }
        other => panic!("expected Custom command, got {other:?}"),
    }

    let parsed_pre_pr = SlashCommand::parse_with_commands("/pre-pr", &assets.commands)
        .expect("pre-pr should parse")
        .expect("pre-pr should resolve to a command");
    assert!(matches!(parsed_pre_pr, SlashCommand::Custom { name, .. } if name == "pre-pr"));
}

/// Second run must not clobber a file the user has since edited: idempotent
/// re-runs preserve user edits exactly, byte for byte.
#[test]
fn init_harness_is_idempotent_and_preserves_user_edits() {
    let workspace = unique_temp_dir("idempotent");
    fs::create_dir_all(&workspace).expect("workspace should exist");

    assert_success(&run_init_harness(&workspace));

    let qas_path = workspace.join(".claw/agents/qas.md");
    let edited =
        "---\nname: qas\ndescription: user-customized gate\nrole: gate\n---\nUser's own words.\n";
    fs::write(&qas_path, edited).expect("user edit should write");

    // Re-run init --harness: must be a no-op for files that already exist.
    assert_success(&run_init_harness(&workspace));

    assert_eq!(
        fs::read_to_string(&qas_path).expect("read qas.md"),
        edited,
        "re-running init --harness must not clobber a user-edited file"
    );
}

/// A file deleted after the first run should be filled back in by a second
/// run, without disturbing any other file.
#[test]
fn init_harness_fills_in_a_deleted_file() {
    let workspace = unique_temp_dir("fill-gap");
    fs::create_dir_all(&workspace).expect("workspace should exist");

    assert_success(&run_init_harness(&workspace));

    let pre_pr_path = workspace.join(".claw/commands/pre-pr.md");
    fs::remove_file(&pre_pr_path).expect("delete pre-pr.md");
    assert!(!pre_pr_path.exists());

    let start_work_path = workspace.join(".claw/commands/start-work.md");
    let start_work_before =
        fs::read_to_string(&start_work_path).expect("read start-work.md before second run");

    assert_success(&run_init_harness(&workspace));

    assert!(
        pre_pr_path.is_file(),
        "deleted pre-pr.md should be recreated by a second init --harness run"
    );
    assert_eq!(
        fs::read_to_string(&start_work_path).expect("read start-work.md after second run"),
        start_work_before,
        "untouched files must not change on a gap-filling re-run"
    );
}

/// A pre-existing `.claw.json` must be merged into (preserving other keys)
/// rather than overwritten, and only gains `hooks.SessionStart` when it
/// doesn't already have one.
#[test]
fn init_harness_merges_into_existing_claw_json_preserving_other_keys() {
    let workspace = unique_temp_dir("merge-claw-json");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(
        workspace.join(".claw.json"),
        r#"{"permissions":{"defaultMode":"acceptEdits"},"someOtherKey":"keep-me"}"#,
    )
    .expect("write pre-existing .claw.json");

    assert_success(&run_init_harness(&workspace));

    let merged: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join(".claw.json")).expect("read merged .claw.json"),
    )
    .expect("merged .claw.json should be valid JSON");

    assert_eq!(
        merged.get("someOtherKey").and_then(|v| v.as_str()),
        Some("keep-me"),
        "merge must preserve unrelated existing keys"
    );
    assert_eq!(
        merged
            .get("permissions")
            .and_then(|p| p.get("defaultMode"))
            .and_then(|v| v.as_str()),
        Some("acceptEdits"),
        "merge must preserve the existing permissions block"
    );
    assert!(
        merged
            .get("hooks")
            .and_then(|hooks| hooks.get("SessionStart"))
            .and_then(|v| v.as_array())
            .is_some(),
        "merge must add hooks.SessionStart"
    );
}

/// An existing `hooks.SessionStart` must never be touched, even across
/// re-runs -- the merge only fills in the key when it's missing.
#[test]
fn init_harness_never_overwrites_an_existing_session_start_hook() {
    let workspace = unique_temp_dir("preserve-session-start");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(
        workspace.join(".claw.json"),
        r#"{"hooks":{"SessionStart":["echo already-configured"]}}"#,
    )
    .expect("write pre-existing .claw.json");

    assert_success(&run_init_harness(&workspace));

    let merged: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(workspace.join(".claw.json")).expect("read .claw.json"),
    )
    .expect("valid JSON");
    let session_start = merged
        .get("hooks")
        .and_then(|hooks| hooks.get("SessionStart"))
        .and_then(|v| v.as_array())
        .expect("hooks.SessionStart should still be present");
    assert_eq!(session_start.len(), 1);
    assert_eq!(
        session_start[0].as_str(),
        Some("echo already-configured"),
        "an existing SessionStart hook must not be modified or appended to"
    );
}
