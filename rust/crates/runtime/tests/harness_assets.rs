//! Integration tests for `.claw` harness asset discovery (skills/commands/agents).

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use runtime::harness_assets::{discover, AgentRole};

fn write_md(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// `discover()` reads the process-wide `CLAW_CONFIG_HOME` env var, and
/// `cargo test` runs tests in this file on multiple threads by default.
/// Serialize all tests in this file so the env mutation in
/// `project_level_shadows_user_level_on_name_clash` can't race with the
/// others reading (or failing to read) that variable.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn discovers_all_three_asset_kinds_from_project_dir() {
    let _guard = env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();

    write_md(
        &cwd.join(".claw/skills/deploy-sop/SKILL.md"),
        "---\nname: deploy-sop\ndescription: Deploy standard operating procedure\n---\nBody text.\n",
    );
    write_md(
        &cwd.join(".claw/commands/start-work.md"),
        "---\nname: start-work\ndescription: Start a work session\nargument-hint: <task-id>\n---\nBody.\n",
    );
    write_md(
        &cwd.join(".claw/agents/qas.md"),
        "---\nname: qas\ndescription: QA subagent\nrole: gate\n---\nBody.\n",
    );

    let assets = discover(cwd);

    assert_eq!(assets.skills.len(), 1);
    assert_eq!(assets.skills[0].name, "deploy-sop");
    assert_eq!(
        assets.skills[0].description,
        "Deploy standard operating procedure"
    );
    assert_eq!(
        assets.skills[0].path,
        cwd.join(".claw/skills/deploy-sop/SKILL.md")
    );

    assert_eq!(assets.commands.len(), 1);
    assert_eq!(assets.commands[0].name, "start-work");
    assert_eq!(assets.commands[0].description, "Start a work session");
    assert_eq!(
        assets.commands[0].argument_hint.as_deref(),
        Some("<task-id>")
    );

    assert_eq!(assets.agents.len(), 1);
    assert_eq!(assets.agents[0].name, "qas");
    assert_eq!(assets.agents[0].description, "QA subagent");
    assert_eq!(assets.agents[0].role, AgentRole::Gate);
}

#[test]
fn project_level_shadows_user_level_on_name_clash() {
    let _guard = env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().join("project");
    let user_home = temp.path().join("user-config");
    fs::create_dir_all(&cwd).expect("mkdir cwd");
    fs::create_dir_all(&user_home).expect("mkdir user home");

    // Safety: test process env mutation; no other test in this binary reads
    // CLAW_CONFIG_HOME concurrently since integration test binaries run
    // single-threaded per file by default unless explicitly parallelized.
    std::env::set_var("CLAW_CONFIG_HOME", &user_home);

    write_md(
        &user_home.join("skills/deploy-sop/SKILL.md"),
        "---\nname: deploy-sop\ndescription: USER LEVEL VERSION\n---\nBody.\n",
    );
    write_md(
        &cwd.join(".claw/skills/deploy-sop/SKILL.md"),
        "---\nname: deploy-sop\ndescription: PROJECT LEVEL VERSION\n---\nBody.\n",
    );

    let assets = discover(&cwd);

    std::env::remove_var("CLAW_CONFIG_HOME");

    let matches: Vec<_> = assets
        .skills
        .iter()
        .filter(|skill| skill.name == "deploy-sop")
        .collect();
    assert_eq!(matches.len(), 1, "project should shadow user-level asset");
    assert_eq!(matches[0].description, "PROJECT LEVEL VERSION");
}

#[test]
fn malformed_frontmatter_is_skipped_with_warning() {
    let _guard = env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();

    write_md(
        &cwd.join(".claw/skills/broken/SKILL.md"),
        "no frontmatter here, just body text\n",
    );
    write_md(
        &cwd.join(".claw/skills/good/SKILL.md"),
        "---\nname: good\ndescription: A good skill\n---\nBody.\n",
    );

    let result = discover(cwd);

    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "good");
    assert!(
        !result.warnings.is_empty(),
        "malformed frontmatter should produce a collectable warning"
    );
    assert!(result.warnings.iter().any(|w| w.contains("broken")));
}

#[test]
fn role_parsing_handles_gate_default_and_unknown() {
    let _guard = env_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();

    write_md(
        &cwd.join(".claw/agents/gate-agent.md"),
        "---\nname: gate-agent\ndescription: A gate agent\nrole: gate\n---\nBody.\n",
    );
    write_md(
        &cwd.join(".claw/agents/default-agent.md"),
        "---\nname: default-agent\ndescription: No role specified\n---\nBody.\n",
    );
    write_md(
        &cwd.join(".claw/agents/unknown-role-agent.md"),
        "---\nname: unknown-role-agent\ndescription: Bad role\nrole: wizard\n---\nBody.\n",
    );

    let result = discover(cwd);

    let find = |name: &str| {
        result
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .unwrap_or_else(|| panic!("expected agent {name} to be discovered"))
    };

    assert_eq!(find("gate-agent").role, AgentRole::Gate);
    assert_eq!(find("default-agent").role, AgentRole::Implementer);
    assert_eq!(find("unknown-role-agent").role, AgentRole::Implementer);
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("unknown-role-agent") || w.to_lowercase().contains("role")),
        "unknown role should produce a warning: {:?}",
        result.warnings
    );
}
