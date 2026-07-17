# SkillWeaver Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `clawcli` a SkillWeaver-style self-improvement loop — the agent distills successful session trajectories into reusable skills, tracks each learned skill's success rate, and refines or quarantines skills that fail — modeled on the SkillWeaver paper ([arXiv:2504.07079](https://arxiv.org/abs/2504.07079)): propose → synthesize → hone → transfer.

**Architecture:** A new `skill_weaver` module in `crates/runtime` mirrors the existing `dreamer.rs` pattern (provider call over local logs, lock file, auto-run gate, atomic writes). It reads finished session JSONL files, asks the configured provider to synthesize `SKILL.md` files into `.claw/skills/learned/`, and keeps a per-skill outcome ledger at `.claw/skills/.weaver/stats.json`. The existing `harness_assets::discover` picks learned skills up automatically (they live under `.claw/skills/`), so the system prompt index, the `Skill` tool, and `/skills` all work with zero changes to their contracts. Honing runs on the ledger: skills that fail repeatedly get quarantined out of the discovery path.

**Tech Stack:** Rust (existing workspace), `serde_json` for the ledger, existing `ApiClient`/`ApiRequest` provider abstraction from `crates/runtime/src/conversation.rs`. No new external crates.

## Global Constraints

- Verification commands (from `rust/`): `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; formatting via `scripts/fmt.sh` from repo root.
- `tools` depends on `runtime`, never the reverse (documented in `harness_assets.rs`). All new provider-calling logic lives in `runtime`.
- Learned skills are written under `.claw/skills/learned/<name>/SKILL.md` so existing discovery (`harness_assets::discover`) and `resolve_skill_path` find them with no lookup-root changes.
- Quarantined skills move to `.claw/skills/.weaver/quarantine/<name>/SKILL.md` — outside any `skills/` walk? No: `.weaver` **is** under `skills/`, so quarantine files must **not** end in `.md`-discoverable form; we rename `SKILL.md` → `SKILL.md.quarantined` on quarantine (discovery only collects `*.md`).
- All file writes are atomic (temp file + rename), matching `dreamer.rs`.
- Weaver never runs concurrently with itself: lock file `.claw/skills/.weaver/.weave-lock`.
- Auto-weave gate: minimum 24h between passes, minimum 3 completed sessions since last pass, off by default (config opt-in), mirroring `AUTO_DREAM_MIN_INTERVAL` semantics.
- Skill frontmatter contract (must parse with `harness_assets::parse_frontmatter_value`): `---\nname: <kebab>\ndescription: <one line>\n---`.

## File Structure

- Create: `rust/crates/runtime/src/skill_weaver.rs` — ledger, gates, episode collection, weave pass, honing. One module, one responsibility: the self-improvement loop. (~600 lines, in line with `dreamer.rs`.)
- Modify: `rust/crates/runtime/src/lib.rs` — export `pub mod skill_weaver;`.
- Modify: `rust/crates/runtime/src/config.rs` — add `WeaverConfig` (enabled flag, learned-dir override, thresholds).
- Modify: `rust/crates/tools/src/lib.rs` — `execute_skill` records an invocation event to the ledger (fire-and-forget).
- Modify: `rust/crates/commands/src/lib.rs` — `/skills weave`, `/skills stats`, `/skills quarantine <name>`, `/skills restore <name>` subcommands.
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` — wire slash dispatch to the weaver functions with the live `ApiClient`; run auto-weave gate check at session end alongside `maybe_run_auto_dream`.
- Tests live next to code (`#[cfg(test)]` modules) plus integration test `rust/crates/runtime/tests/skill_weaver_pass.rs`.

---

### Task 1: Skill outcome ledger

**Files:**
- Create: `rust/crates/runtime/src/skill_weaver.rs`
- Modify: `rust/crates/runtime/src/lib.rs` (add `pub mod skill_weaver;` next to `pub mod dreamer;`)

**Interfaces:**
- Produces: `SkillLedger::load(weaver_dir: &Path) -> SkillLedger`, `SkillLedger::record(&mut self, skill: &str, outcome: SkillOutcome)`, `SkillLedger::save(&self, weaver_dir: &Path) -> io::Result<PathBuf>`, `SkillLedger::entry(&self, skill: &str) -> Option<&SkillRecord>`, `pub fn weaver_dir(cwd: &Path) -> PathBuf` (returns `<cwd>/.claw/skills/.weaver`), `pub enum SkillOutcome { Invoked, Success, Failure }`, `pub struct SkillRecord { pub invocations: u64, pub successes: u64, pub failures: u64, pub last_used_ms: u64 }`.

- [ ] **Step 1: Write the failing tests**

In `skill_weaver.rs`, module skeleton plus tests:

```rust
//! SkillWeaver-style self-improvement: distill session trajectories into
//! learned skills, track per-skill outcomes, refine or quarantine failures.
//! Modeled on dreamer.rs (locks, gates, atomic writes, provider pass).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const WEAVER_DIR_NAME: &str = ".weaver";
pub const STATS_FILENAME: &str = "stats.json";

/// `<cwd>/.claw/skills/.weaver`
pub fn weaver_dir(cwd: &Path) -> PathBuf {
    cwd.join(".claw").join("skills").join(WEAVER_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_roundtrips_and_accumulates() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        let mut ledger = SkillLedger::load(&weaver);
        ledger.record("learned/fix-clippy", SkillOutcome::Invoked);
        ledger.record("learned/fix-clippy", SkillOutcome::Success);
        ledger.record("learned/fix-clippy", SkillOutcome::Failure);
        ledger.save(&weaver).unwrap();

        let reloaded = SkillLedger::load(&weaver);
        let record = reloaded.entry("learned/fix-clippy").unwrap();
        assert_eq!(record.invocations, 1);
        assert_eq!(record.successes, 1);
        assert_eq!(record.failures, 1);
        assert!(record.last_used_ms > 0);
    }

    #[test]
    fn ledger_load_missing_or_corrupt_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        assert!(SkillLedger::load(&weaver).is_empty());
        fs::create_dir_all(&weaver).unwrap();
        fs::write(weaver.join(STATS_FILENAME), "{not json").unwrap();
        assert!(SkillLedger::load(&weaver).is_empty());
    }
}
```

(Check `rust/crates/runtime/Cargo.toml` for `tempfile` in `[dev-dependencies]`; `dreamer.rs` tests already use temp dirs, so match whatever helper that file uses if `tempfile` is absent.)

- [ ] **Step 2: Run tests to verify they fail**

Run (from `rust/`): `cargo test -p claw-runtime skill_weaver -- --nocapture` (substitute the actual runtime crate name from `crates/runtime/Cargo.toml` if it differs).
Expected: FAIL — `SkillLedger` not found.

- [ ] **Step 3: Implement the ledger**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOutcome {
    Invoked,
    Success,
    Failure,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillRecord {
    pub invocations: u64,
    pub successes: u64,
    pub failures: u64,
    pub last_used_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillLedger {
    #[serde(default)]
    skills: BTreeMap<String, SkillRecord>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SkillLedger {
    /// Missing or corrupt stats files load as an empty ledger — the ledger
    /// is advisory telemetry, never a hard dependency.
    pub fn load(weaver_dir: &Path) -> SkillLedger {
        let path = weaver_dir.join(STATS_FILENAME);
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => SkillLedger::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn entry(&self, skill: &str) -> Option<&SkillRecord> {
        self.skills.get(skill)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &SkillRecord)> {
        self.skills.iter()
    }

    pub fn record(&mut self, skill: &str, outcome: SkillOutcome) {
        let record = self.skills.entry(skill.to_string()).or_default();
        match outcome {
            SkillOutcome::Invoked => record.invocations += 1,
            SkillOutcome::Success => record.successes += 1,
            SkillOutcome::Failure => record.failures += 1,
        }
        record.last_used_ms = now_ms();
    }

    pub fn save(&self, weaver_dir: &Path) -> io::Result<PathBuf> {
        fs::create_dir_all(weaver_dir)?;
        let dest = weaver_dir.join(STATS_FILENAME);
        let tmp = weaver_dir.join(format!("{STATS_FILENAME}.tmp"));
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, raw)?;
        fs::rename(&tmp, &dest)?;
        Ok(dest)
    }
}
```

Add `pub mod skill_weaver;` to `rust/crates/runtime/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-runtime skill_weaver`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs rust/crates/runtime/src/lib.rs
git commit -m "feat(runtime): add skill outcome ledger for skill-weaver"
```

---

### Task 2: Episode collection from session logs

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs`

**Interfaces:**
- Consumes: `crate::session::workspace_sessions_dir(cwd)` (`session.rs:2299`) and session JSONL files it contains.
- Produces: `pub struct Episode { pub session_file: PathBuf, pub content: String, pub modified: SystemTime }`, `pub fn collect_episodes(cwd: &Path, since: Option<SystemTime>, max_total_bytes: usize) -> Result<Vec<Episode>, WeaverError>`, `pub enum WeaverError { Io(io::Error), Api(String), InvalidOutput(String), NoEpisodes, Locked }` with `Display` + `From<io::Error>` impls.

Episodes are the raw material the provider distills. Collection is intentionally dumb: newest-first session files since the last weave, truncated to a byte budget — the provider does the "was this a reusable pattern?" judgment (this mirrors how `collect_memory_logs` feeds the dreamer, `dreamer.rs:324`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn collect_episodes_reads_newest_first_and_respects_budget() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("a.jsonl"), "old session\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(sessions.join("b.jsonl"), "new session\n").unwrap();

    let episodes = collect_episodes(dir.path(), None, 1024).unwrap();
    assert_eq!(episodes.len(), 2);
    assert!(episodes[0].content.contains("new session"));

    // Budget smaller than both files keeps only the newest.
    let episodes = collect_episodes(dir.path(), None, "new session\n".len()).unwrap();
    assert_eq!(episodes.len(), 1);
}

#[test]
fn collect_episodes_since_filters_old_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("a.jsonl"), "old\n").unwrap();
    let cutoff = SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(sessions.join("b.jsonl"), "new\n").unwrap();

    let episodes = collect_episodes(dir.path(), Some(cutoff), 1024).unwrap();
    assert_eq!(episodes.len(), 1);
    assert!(episodes[0].content.contains("new"));
}
```

Note: if `workspace_sessions_dir` derives its location from something other than `cwd` (e.g. a hashed global dir), read its implementation at `session.rs:2299` first and adapt the test to create files where it actually looks — the function under test must use `workspace_sessions_dir` as its single source of truth either way.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claw-runtime skill_weaver`
Expected: FAIL — `collect_episodes` not found.

- [ ] **Step 3: Implement collection**

```rust
#[derive(Debug)]
pub enum WeaverError {
    Io(io::Error),
    Api(String),
    InvalidOutput(String),
    NoEpisodes,
    Locked,
}

impl std::fmt::Display for WeaverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeaverError::Io(e) => write!(f, "io error: {e}"),
            WeaverError::Api(e) => write!(f, "provider error: {e}"),
            WeaverError::InvalidOutput(e) => write!(f, "invalid weaver output: {e}"),
            WeaverError::NoEpisodes => write!(f, "no session episodes to weave"),
            WeaverError::Locked => write!(f, "another weave pass is running"),
        }
    }
}

impl std::error::Error for WeaverError {}

impl From<io::Error> for WeaverError {
    fn from(e: io::Error) -> Self {
        WeaverError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct Episode {
    pub session_file: PathBuf,
    pub content: String,
    pub modified: SystemTime,
}

pub fn collect_episodes(
    cwd: &Path,
    since: Option<SystemTime>,
    max_total_bytes: usize,
) -> Result<Vec<Episode>, WeaverError> {
    let sessions_dir = crate::session::workspace_sessions_dir(cwd)
        .map_err(|e| WeaverError::Io(io::Error::other(e.to_string())))?;
    let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();
    let entries = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            if since.is_none_or(|cutoff| modified > cutoff) {
                files.push((path, modified));
            }
        }
    }
    // Newest first.
    files.sort_by(|a, b| b.1.cmp(&a.1));

    let mut episodes = Vec::new();
    let mut budget = max_total_bytes;
    for (path, modified) in files {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if content.len() > budget {
            break;
        }
        budget -= content.len();
        episodes.push(Episode {
            session_file: path,
            content,
            modified,
        });
    }
    Ok(episodes)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-runtime skill_weaver`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs
git commit -m "feat(runtime): collect session episodes for skill weaving"
```

---

### Task 3: Weave pass — provider synthesizes learned skills

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs`
- Test: `rust/crates/runtime/tests/skill_weaver_pass.rs`

**Interfaces:**
- Consumes: `Episode`, `WeaverError`, `weaver_dir` (Tasks 1–2); `ApiClient`, `ApiRequest`, `ContentBlock`, `ConversationMessage`, `MessageRole` from `crate::conversation` / `crate::session` (same imports as `dreamer.rs:18`); `harness_assets::parse_frontmatter_value` for output validation.
- Produces: `pub struct WovenSkill { pub name: String, pub markdown: String }`, `pub fn synthesize_skills(episodes: &[Episode], existing_skill_names: &[String], client: &mut impl ApiClient) -> Result<Vec<WovenSkill>, WeaverError>`, `pub fn write_learned_skills(skills: &[WovenSkill], skills_root: &Path) -> Result<Vec<PathBuf>, WeaverError>` (writes `<skills_root>/learned/<name>/SKILL.md`), `pub struct WeaveRun { pub files_written: Vec<PathBuf>, pub episode_count: usize }`, `pub fn run_weave_pass(cwd: &Path, client: &mut impl ApiClient, max_input_bytes: usize) -> Result<WeaveRun, WeaverError>` (lock → collect → synthesize → write → touch `.last-weave` marker).

The provider prompt is the heart of the SkillWeaver adaptation. The paper's pipeline is propose → synthesize → hone; here, proposal and synthesis happen in one provider pass over episodes (proposal as a separate exploration phase makes sense for web agents exploring a site; for a coding CLI, completed sessions *are* the exploration), and honing is Task 4.

- [ ] **Step 1: Write the failing tests**

In-module test for output parsing/validation, plus integration test with a scripted `ApiClient`. First check how `dreamer.rs` tests fake `ApiClient` (search `crates/runtime/src/dreamer.rs` and `crates/runtime/tests/` for `impl ApiClient`) and reuse that pattern. Integration test `rust/crates/runtime/tests/skill_weaver_pass.rs`:

```rust
use std::fs;

// Reuse the mock ApiClient pattern from the dreamer tests; the mock returns
// `MOCK_RESPONSE` as a single text event.
const MOCK_RESPONSE: &str = r#"<skill name="fix-clippy-warnings">
---
name: fix-clippy-warnings
description: Run clippy with -D warnings and fix each lint mechanically before committing
---

# Fix clippy warnings

1. Run `cargo clippy --workspace --all-targets -- -D warnings`.
2. Fix each warning in source order; re-run until clean.
</skill>"#;

#[test]
fn weave_pass_writes_learned_skill_and_marker() {
    let dir = tempfile::tempdir().unwrap();
    // Arrange one session file (same location contract as Task 2 tests).
    let sessions = claw_runtime::session::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("s1.jsonl"), "user asked to fix clippy; agent ran clippy, fixed, tests passed\n").unwrap();

    let mut client = mock_client_returning(MOCK_RESPONSE);
    let run = claw_runtime::skill_weaver::run_weave_pass(dir.path(), &mut client, 64 * 1024).unwrap();

    assert_eq!(run.episode_count, 1);
    let skill_path = dir
        .path()
        .join(".claw/skills/learned/fix-clippy-warnings/SKILL.md");
    assert!(skill_path.is_file());
    let contents = fs::read_to_string(&skill_path).unwrap();
    assert!(contents.starts_with("---"));
    // Discovery must see it.
    let assets = claw_runtime::harness_assets::discover(dir.path());
    assert!(assets.skills.iter().any(|s| s.name == "fix-clippy-warnings"));
    // Marker written.
    assert!(claw_runtime::skill_weaver::weaver_dir(dir.path()).join(".last-weave").is_file());
}

#[test]
fn weave_pass_rejects_bad_skill_names() {
    // A response with a path-escaping name must be dropped, not written.
    let bad = r#"<skill name="../evil">
---
name: ../evil
description: nope
---
body
</skill>"#;
    let dir = tempfile::tempdir().unwrap();
    let sessions = claw_runtime::session::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("s1.jsonl"), "trajectory\n").unwrap();

    let mut client = mock_client_returning(bad);
    let err = claw_runtime::skill_weaver::run_weave_pass(dir.path(), &mut client, 64 * 1024);
    assert!(err.is_err() || !dir.path().join(".claw/skills/learned").exists());
}
```

(Substitute the actual runtime crate name for `claw_runtime` from `crates/runtime/Cargo.toml`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claw-runtime --test skill_weaver_pass`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement synthesis, writing, and the pass**

System prompt constant (module-level in `skill_weaver.rs`):

```rust
const WEAVER_SYSTEM_PROMPT: &str = r#"You are the Skill Weaver for an agentic coding CLI. You read transcripts of completed agent sessions and distill *reusable, generalizable procedures* into skill files, exactly like the SkillWeaver propose-and-synthesize loop.

Rules:
- Only weave a skill when the transcript shows a NON-OBVIOUS multi-step procedure that succeeded and would plausibly recur (recurring build/test/debug workflow, project-specific incantation, multi-tool recipe).
- Never weave: one-liner commands, generic knowledge the model already has, secrets/credentials, user-specific data, or anything from a failed trajectory.
- Skill names are kebab-case, [a-z0-9-]+ only, 3-40 chars, and must not duplicate an existing skill name from the provided list.
- Each skill is emitted as a block:
<skill name="<kebab-name>">
---
name: <kebab-name>
description: <one line: when to use this skill>
---

# <Title>

<numbered, concrete steps with exact commands where known>
</skill>
- Emit zero skills (empty response) when nothing qualifies. Quality over quantity: at most 3 skills per pass."#;
```

Implementation:

```rust
use crate::conversation::{ApiClient, ApiRequest};
use crate::harness_assets::parse_frontmatter_value;
use crate::session::{ContentBlock, ConversationMessage, MessageRole};

pub const LAST_WEAVE_FILENAME: &str = ".last-weave";
pub const WEAVE_LOCK_FILENAME: &str = ".weave-lock";
pub const LEARNED_DIR_NAME: &str = "learned";
const MAX_SKILLS_PER_PASS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WovenSkill {
    pub name: String,
    pub markdown: String,
}

fn valid_skill_name(name: &str) -> bool {
    (3..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse `<skill name="...">...</skill>` blocks out of the raw provider text.
fn parse_weaver_output(raw: &str) -> Result<Vec<WovenSkill>, WeaverError> {
    let mut skills = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find("<skill name=\"") {
        let after = &rest[start + "<skill name=\"".len()..];
        let Some(name_end) = after.find('"') else { break };
        let name = after[..name_end].to_string();
        let Some(body_start) = after.find('>') else { break };
        let body_rest = &after[body_start + 1..];
        let Some(end) = body_rest.find("</skill>") else {
            return Err(WeaverError::InvalidOutput(format!(
                "unterminated skill block '{name}'"
            )));
        };
        let markdown = body_rest[..end].trim().to_string();
        if !valid_skill_name(&name) {
            return Err(WeaverError::InvalidOutput(format!(
                "invalid skill name '{name}'"
            )));
        }
        let frontmatter_name = parse_frontmatter_value(&markdown, "name");
        if frontmatter_name.as_deref() != Some(name.as_str()) {
            return Err(WeaverError::InvalidOutput(format!(
                "skill '{name}': frontmatter name mismatch"
            )));
        }
        if parse_frontmatter_value(&markdown, "description").is_none() {
            return Err(WeaverError::InvalidOutput(format!(
                "skill '{name}': missing description"
            )));
        }
        skills.push(WovenSkill { name, markdown });
        rest = &body_rest[end + "</skill>".len()..];
    }
    skills.truncate(MAX_SKILLS_PER_PASS);
    Ok(skills)
}

pub fn synthesize_skills(
    episodes: &[Episode],
    existing_skill_names: &[String],
    client: &mut impl ApiClient,
) -> Result<Vec<WovenSkill>, WeaverError> {
    if episodes.is_empty() {
        return Err(WeaverError::NoEpisodes);
    }
    let mut user_message = String::from("Existing skill names (do not duplicate):\n");
    for name in existing_skill_names {
        user_message.push_str("- ");
        user_message.push_str(name);
        user_message.push('\n');
    }
    user_message.push_str("\nSession transcripts, newest first:\n\n");
    for episode in episodes {
        user_message.push_str(&format!(
            "=== session {} ===\n{}\n",
            episode.session_file.display(),
            episode.content
        ));
    }

    let request = ApiRequest {
        system_prompt: vec![WEAVER_SYSTEM_PROMPT.to_string()],
        messages: vec![ConversationMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text { text: user_message }],
            usage: None,
        }],
    };
    let events = client
        .stream(request)
        .map_err(|e| WeaverError::Api(e.to_string()))?;
    // Reuse the same event→text collection as dreamer.rs. If
    // `collect_text_from_events` is private there, lift it into a shared
    // private helper module or duplicate the ~10-line fold here.
    let raw = collect_text_from_events(&events);
    parse_weaver_output(&raw)
}

pub fn write_learned_skills(
    skills: &[WovenSkill],
    skills_root: &Path,
) -> Result<Vec<PathBuf>, WeaverError> {
    let mut written = Vec::new();
    for skill in skills {
        let dir = skills_root.join(LEARNED_DIR_NAME).join(&skill.name);
        fs::create_dir_all(&dir)?;
        let dest = dir.join("SKILL.md");
        let tmp = dir.join("SKILL.md.tmp");
        let mut body = skill.markdown.clone();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        fs::write(&tmp, body)?;
        fs::rename(&tmp, &dest)?;
        written.push(dest);
    }
    Ok(written)
}

#[derive(Debug)]
pub struct WeaveRun {
    pub files_written: Vec<PathBuf>,
    pub episode_count: usize,
}

pub fn run_weave_pass(
    cwd: &Path,
    client: &mut impl ApiClient,
    max_input_bytes: usize,
) -> Result<WeaveRun, WeaverError> {
    let weaver = weaver_dir(cwd);
    fs::create_dir_all(&weaver)?;
    let _lock = WeaveLock::try_acquire(&weaver)?; // copy DreamLock's create_new pattern

    let since = last_weave_time(&weaver);
    let episodes = collect_episodes(cwd, since, max_input_bytes)?;
    if episodes.is_empty() {
        return Err(WeaverError::NoEpisodes);
    }
    let existing: Vec<String> = crate::harness_assets::discover(cwd)
        .skills
        .into_iter()
        .map(|s| s.name)
        .collect();
    let skills = synthesize_skills(&episodes, &existing, client)?;
    let skills_root = cwd.join(".claw").join("skills");
    let files_written = write_learned_skills(&skills, &skills_root)?;
    fs::write(weaver.join(LAST_WEAVE_FILENAME), b"")?;
    Ok(WeaveRun {
        files_written,
        episode_count: episodes.len(),
    })
}

fn last_weave_time(weaver: &Path) -> Option<SystemTime> {
    fs::metadata(weaver.join(LAST_WEAVE_FILENAME))
        .and_then(|m| m.modified())
        .ok()
}
```

`WeaveLock`: copy `DreamLock` from `dreamer.rs` verbatim (RAII guard: `OpenOptions::new().write(true).create_new(true)` on `WEAVE_LOCK_FILENAME`, remove on `Drop`, map `AlreadyExists` → `WeaverError::Locked`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-runtime --test skill_weaver_pass && cargo test -p claw-runtime skill_weaver`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs rust/crates/runtime/tests/skill_weaver_pass.rs
git commit -m "feat(runtime): weave pass synthesizes learned skills from sessions"
```

---

### Task 4: Honing — quarantine failing skills, restore, prompt-visible stats

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs`

**Interfaces:**
- Consumes: `SkillLedger`, `SkillRecord`, `weaver_dir` (Task 1); learned-skill layout (Task 3).
- Produces: `pub fn hone(cwd: &Path, ledger: &SkillLedger) -> Result<Vec<String>, WeaverError>` (returns names quarantined), `pub fn quarantine_skill(cwd: &Path, name: &str) -> Result<PathBuf, WeaverError>`, `pub fn restore_skill(cwd: &Path, name: &str) -> Result<PathBuf, WeaverError>`, `pub const QUARANTINE_MIN_INVOCATIONS: u64 = 3;`, `pub const QUARANTINE_MAX_SUCCESS_RATE: f64 = 0.34;`.

SkillWeaver's honing phase practices skills and refines them; the CLI analogue is passive: real usage is the practice, the ledger is the score, and honing demotes skills whose observed success rate is bad. Quarantine = rename `SKILL.md` → `SKILL.md.quarantined` in place (discovery only picks up `*.md`, verified in `harness_assets.rs:224-228`). Only skills under `learned/` are ever auto-quarantined — hand-written skills are never touched.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn hone_quarantines_failing_learned_skill_and_restore_reverses() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join(".claw/skills/learned/flaky-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: flaky-skill\ndescription: test\n---\nbody\n",
    )
    .unwrap();

    let mut ledger = SkillLedger::load(&weaver_dir(dir.path()));
    for _ in 0..3 {
        ledger.record("flaky-skill", SkillOutcome::Invoked);
        ledger.record("flaky-skill", SkillOutcome::Failure);
    }

    let quarantined = hone(dir.path(), &ledger).unwrap();
    assert_eq!(quarantined, vec!["flaky-skill".to_string()]);
    assert!(!skill_dir.join("SKILL.md").exists());
    assert!(skill_dir.join("SKILL.md.quarantined").exists());
    // Discovery no longer sees it.
    let assets = crate::harness_assets::discover(dir.path());
    assert!(!assets.skills.iter().any(|s| s.name == "flaky-skill"));

    restore_skill(dir.path(), "flaky-skill").unwrap();
    assert!(skill_dir.join("SKILL.md").exists());
}

#[test]
fn hone_never_touches_hand_written_or_low_sample_skills() {
    let dir = tempfile::tempdir().unwrap();
    // Hand-written skill outside learned/.
    let hand = dir.path().join(".claw/skills/manual-skill");
    fs::create_dir_all(&hand).unwrap();
    fs::write(hand.join("SKILL.md"), "---\nname: manual-skill\ndescription: x\n---\nbody\n").unwrap();
    // Learned skill with too few invocations.
    let fresh = dir.path().join(".claw/skills/learned/fresh-skill");
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("SKILL.md"), "---\nname: fresh-skill\ndescription: x\n---\nbody\n").unwrap();

    let mut ledger = SkillLedger::load(&weaver_dir(dir.path()));
    ledger.record("manual-skill", SkillOutcome::Invoked);
    ledger.record("manual-skill", SkillOutcome::Failure);
    ledger.record("fresh-skill", SkillOutcome::Invoked);
    ledger.record("fresh-skill", SkillOutcome::Failure);

    let quarantined = hone(dir.path(), &ledger).unwrap();
    assert!(quarantined.is_empty());
    assert!(hand.join("SKILL.md").exists());
    assert!(fresh.join("SKILL.md").exists());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claw-runtime skill_weaver`
Expected: FAIL — `hone` not found.

- [ ] **Step 3: Implement honing**

```rust
pub const QUARANTINE_MIN_INVOCATIONS: u64 = 3;
pub const QUARANTINE_MAX_SUCCESS_RATE: f64 = 0.34;
const QUARANTINE_SUFFIX: &str = "quarantined";

fn learned_skill_file(cwd: &Path, name: &str) -> Option<PathBuf> {
    if !valid_skill_name(name) {
        return None;
    }
    Some(
        cwd.join(".claw")
            .join("skills")
            .join(LEARNED_DIR_NAME)
            .join(name)
            .join("SKILL.md"),
    )
}

pub fn quarantine_skill(cwd: &Path, name: &str) -> Result<PathBuf, WeaverError> {
    let live = learned_skill_file(cwd, name)
        .ok_or_else(|| WeaverError::InvalidOutput(format!("invalid skill name '{name}'")))?;
    if !live.is_file() {
        return Err(WeaverError::InvalidOutput(format!(
            "no learned skill '{name}' to quarantine"
        )));
    }
    let parked = live.with_extension(format!("md.{QUARANTINE_SUFFIX}"));
    fs::rename(&live, &parked)?;
    Ok(parked)
}

pub fn restore_skill(cwd: &Path, name: &str) -> Result<PathBuf, WeaverError> {
    let live = learned_skill_file(cwd, name)
        .ok_or_else(|| WeaverError::InvalidOutput(format!("invalid skill name '{name}'")))?;
    let parked = live.with_extension(format!("md.{QUARANTINE_SUFFIX}"));
    if !parked.is_file() {
        return Err(WeaverError::InvalidOutput(format!(
            "no quarantined skill '{name}' to restore"
        )));
    }
    fs::rename(&parked, &live)?;
    Ok(live)
}

/// Quarantine learned skills whose observed success rate is unacceptable.
/// Hand-written skills (outside `learned/`) are never touched: the guard is
/// structural — we only ever rename files under `learned/<name>/`.
pub fn hone(cwd: &Path, ledger: &SkillLedger) -> Result<Vec<String>, WeaverError> {
    let mut quarantined = Vec::new();
    for (name, record) in ledger.iter() {
        if record.invocations < QUARANTINE_MIN_INVOCATIONS {
            continue;
        }
        let judged = record.successes + record.failures;
        if judged == 0 {
            continue;
        }
        let rate = record.successes as f64 / judged as f64;
        if rate > QUARANTINE_MAX_SUCCESS_RATE {
            continue;
        }
        // Structural guard: only learned skills have a file here.
        if learned_skill_file(cwd, name).is_some_and(|p| p.is_file()) {
            quarantine_skill(cwd, name)?;
            quarantined.push(name.clone());
        }
    }
    Ok(quarantined)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-runtime skill_weaver`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs
git commit -m "feat(runtime): hone learned skills via quarantine/restore on ledger stats"
```

---

### Task 5: Skill tool records invocations to the ledger

**Files:**
- Modify: `rust/crates/tools/src/lib.rs` (`execute_skill`, around `tools/src/lib.rs:4140`)

**Interfaces:**
- Consumes: `runtime::skill_weaver::{SkillLedger, SkillOutcome, weaver_dir}` (Task 1). `tools` already depends on `runtime` (see `crates/tools/Cargo.toml`), so the import direction is legal.
- Produces: every successful `Skill` tool call bumps `invocations` for that skill name in the workspace ledger. Success/failure outcomes are recorded by `/skills stats mark` in Task 6 (explicit) — automatic outcome attribution is out of scope for this plan (YAGNI until there's a reliable turn-level success signal).

- [ ] **Step 1: Write the failing test**

In the existing `#[cfg(test)]` module of `tools/src/lib.rs`, next to the current `execute_skill` tests (find them by searching `fn execute_skill` usages in the test module; follow their setup pattern for creating a skill file and cwd):

```rust
#[test]
fn skill_invocation_is_recorded_in_weaver_ledger() {
    // Follow the existing execute_skill test setup for temp cwd + skill file.
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join(".claw/skills/learned/demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nbody\n",
    )
    .unwrap();

    // Use whatever cwd-override mechanism the existing execute_skill tests
    // use (env guard / with_cwd helper) — do not add a new one.
    let _guard = set_cwd_for_test(dir.path());
    let output = execute_skill(SkillInput {
        skill: "demo-skill".to_string(),
    })
    .unwrap();
    assert_eq!(output.skill, "demo-skill");

    let ledger = runtime::skill_weaver::SkillLedger::load(
        &runtime::skill_weaver::weaver_dir(dir.path()),
    );
    assert_eq!(ledger.entry("demo-skill").unwrap().invocations, 1);
}
```

(Adapt `set_cwd_for_test` and the `runtime::` crate name to what the file actually uses — read the neighboring tests first.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p claw-tools skill_invocation_is_recorded` (substitute actual tools crate name).
Expected: FAIL — ledger entry missing.

- [ ] **Step 3: Implement recording in `execute_skill`**

In `execute_skill` (`tools/src/lib.rs:4140`), after the skill file loads successfully, before returning:

```rust
fn record_skill_invocation(skill: &str) {
    // Fire-and-forget telemetry: ledger failures must never fail the tool.
    let Ok(cwd) = std::env::current_dir() else { return };
    let weaver = runtime::skill_weaver::weaver_dir(&cwd);
    let mut ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
    ledger.record(skill, runtime::skill_weaver::SkillOutcome::Invoked);
    let _ = ledger.save(&weaver);
}
```

Call `record_skill_invocation(&input.skill);` inside `execute_skill` right before the `Ok(SkillOutput { ... })`. Note: if `execute_skill` resolves cwd through a parameter or helper instead of `std::env::current_dir()` (check `resolve_skill_path` at `tools/src/lib.rs:4176`), use that same cwd source.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-tools skill_invocation_is_recorded && cargo test -p claw-tools`
Expected: all pass (no regressions in existing skill tests).

- [ ] **Step 5: Commit**

```bash
git add rust/crates/tools/src/lib.rs
git commit -m "feat(tools): record Skill tool invocations in weaver ledger"
```

---

### Task 6: `/skills weave|stats|quarantine|restore|mark` subcommands

**Files:**
- Modify: `rust/crates/commands/src/lib.rs` (extend the existing `skills` command: parser near `commands/src/lib.rs:1479`, `parse_skills_args` at `:1996`, help/usage renderers near `:2889`)
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` (dispatch `weave` with the live `ApiClient`)

**Interfaces:**
- Consumes: `run_weave_pass`, `hone`, `quarantine_skill`, `restore_skill`, `SkillLedger`, `SkillOutcome`, `weaver_dir` (Tasks 1–4).
- Produces: new `/skills` verbs. `weave` needs a provider client, which the `commands` crate doesn't own — so `commands` parses it into the existing `SkillSlashDispatch`-style enum (`commands/src/lib.rs:57`) and the CLI crate executes it, the same split the codebase already uses for skill invocation. `stats`, `quarantine <name>`, `restore <name>`, `mark <name> success|failure` are pure-filesystem and execute inside `commands` directly.

Subcommand behavior:
- `/skills weave` — run a weave pass now (respects lock; reports files written or "no episodes").
- `/skills stats` — render ledger as a table: name, invocations, successes, failures, rate, quarantined?.
- `/skills quarantine <name>` / `/skills restore <name>` — manual honing controls.
- `/skills mark <name> success|failure` — explicit outcome feedback (the honing signal until automatic attribution exists).

- [ ] **Step 1: Write the failing tests**

Follow the existing test conventions in `commands/src/lib.rs` (see the tests around `:6923` that exercise `/skills` rendering). Add:

```rust
#[test]
fn skills_stats_renders_ledger_table() {
    let dir = tempfile::tempdir().unwrap();
    let weaver = runtime::skill_weaver::weaver_dir(dir.path());
    let mut ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
    ledger.record("learned-a", runtime::skill_weaver::SkillOutcome::Invoked);
    ledger.record("learned-a", runtime::skill_weaver::SkillOutcome::Success);
    ledger.save(&weaver).unwrap();

    let report = render_skills_stats(dir.path()).unwrap();
    assert!(report.contains("learned-a"));
    assert!(report.contains("1")); // invocations
}

#[test]
fn skills_mark_records_outcome() {
    let dir = tempfile::tempdir().unwrap();
    run_skills_mark(dir.path(), "some-skill", "failure").unwrap();
    let ledger = runtime::skill_weaver::SkillLedger::load(
        &runtime::skill_weaver::weaver_dir(dir.path()),
    );
    assert_eq!(ledger.entry("some-skill").unwrap().failures, 1);
}

#[test]
fn skills_weave_parses_to_dispatch() {
    // Assert `/skills weave` parses into the dispatch variant rather than
    // erroring — follow the existing SlashCommand::Skills parse tests.
    let parsed = parse_slash_command("/skills weave").unwrap();
    // shape assertion per existing test style
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claw-commands skills_stats skills_mark skills_weave`
Expected: FAIL — functions/variants not found.

- [ ] **Step 3: Implement**

In `commands/src/lib.rs`:

```rust
pub fn render_skills_stats(cwd: &Path) -> Result<String, String> {
    let weaver = runtime::skill_weaver::weaver_dir(cwd);
    let ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
    if ledger.is_empty() {
        return Ok("No skill usage recorded yet.".to_string());
    }
    let mut out = String::from("Skill                          Inv  Ok  Fail  Rate\n");
    for (name, record) in ledger.iter() {
        let judged = record.successes + record.failures;
        let rate = if judged == 0 {
            "-".to_string()
        } else {
            format!("{:.0}%", 100.0 * record.successes as f64 / judged as f64)
        };
        out.push_str(&format!(
            "{name:<30} {inv:>4} {ok:>3} {fail:>5} {rate:>5}\n",
            inv = record.invocations,
            ok = record.successes,
            fail = record.failures,
        ));
    }
    Ok(out)
}

pub fn run_skills_mark(cwd: &Path, skill: &str, verdict: &str) -> Result<String, String> {
    let outcome = match verdict {
        "success" => runtime::skill_weaver::SkillOutcome::Success,
        "failure" => runtime::skill_weaver::SkillOutcome::Failure,
        other => return Err(format!("invalid_argument: expected success|failure, got '{other}'")),
    };
    let weaver = runtime::skill_weaver::weaver_dir(cwd);
    let mut ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
    ledger.record(skill, outcome);
    ledger.save(&weaver).map_err(|e| e.to_string())?;
    Ok(format!("recorded {verdict} for '{skill}'"))
}
```

Wire `quarantine`/`restore` verbs straight to `runtime::skill_weaver::{quarantine_skill, restore_skill}` with their `Display` errors. Extend the `skills` `argument_hint` (`commands/src/lib.rs:260`) to `"[list|show <name>|install <path>|uninstall <name>|weave|stats|quarantine <name>|restore <name>|mark <name> success|failure|help|<skill> [args]]"` and add the verbs to `render_skills_usage`. Add a `Weave` variant to the skills dispatch enum so the CLI layer executes it.

In `rusty-claude-cli/src/main.rs`, where skills dispatch is handled (search for how existing `SkillSlashDispatch` variants are executed), add:

```rust
SkillsDispatch::Weave => {
    match runtime::skill_weaver::run_weave_pass(&cwd, &mut client, 64 * 1024) {
        Ok(run) => {
            let honed = {
                let weaver = runtime::skill_weaver::weaver_dir(&cwd);
                let ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
                runtime::skill_weaver::hone(&cwd, &ledger).unwrap_or_default()
            };
            println!(
                "wove {} skill(s) from {} session(s){}",
                run.files_written.len(),
                run.episode_count,
                if honed.is_empty() {
                    String::new()
                } else {
                    format!("; quarantined: {}", honed.join(", "))
                }
            );
        }
        Err(runtime::skill_weaver::WeaverError::NoEpisodes) => {
            println!("nothing to weave: no new session episodes");
        }
        Err(e) => eprintln!("weave failed: {e}"),
    }
}
```

(Adapt variant names, client access, and output channel to the surrounding dispatch code — mirror how `/memory dream` or the closest provider-invoking command is wired.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claw-commands && cargo test -p rusty-claude-cli`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/commands/src/lib.rs rust/crates/rusty-claude-cli/src/main.rs
git commit -m "feat(commands): /skills weave, stats, quarantine, restore, mark"
```

---

### Task 7: Auto-weave gate + config + docs

**Files:**
- Modify: `rust/crates/runtime/src/config.rs` (add `WeaverConfig` next to `MemoryConfig`)
- Modify: `rust/crates/runtime/src/skill_weaver.rs` (gate + `maybe_run_auto_weave`)
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` (call `maybe_run_auto_weave` at the same lifecycle point as `maybe_run_auto_dream`)
- Modify: `CLAW_CODE/USAGE.md` (document `/skills weave|stats|quarantine|restore|mark`, the learned-skills directory, config keys)

**Interfaces:**
- Consumes: gate pattern from `dreamer.rs:471` (`auto_dream_gate`) and `maybe_run_auto_dream` (`dreamer.rs:512`); `MemoryConfig`'s serde/defaults style in `config.rs`.
- Produces: `pub struct WeaverConfig { pub auto_weave: bool /* default false */, pub max_input_bytes: usize /* default 65536 */ }` parsed from settings under a `"weaver"` key (mirror `MemoryConfig`'s exact serde derive/default style); `pub enum WeaveGate { Disabled, Locked, TooSoon { remaining: Duration }, TooFewSessions { touched: usize, required: usize }, Ready }`; `pub fn auto_weave_gate(weaver: &Path, cwd: &Path, enabled: bool, force: bool) -> Result<WeaveGate, WeaverError>`; `pub fn maybe_run_auto_weave(cwd: &Path, config: &WeaverConfig, client: &mut impl ApiClient) -> Result<Option<WeaveRun>, WeaverError>`; constants `AUTO_WEAVE_MIN_INTERVAL: Duration = 24h`, `AUTO_WEAVE_MIN_TOUCHED_SESSIONS: usize = 3`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn auto_weave_gate_disabled_and_too_soon() {
    let dir = tempfile::tempdir().unwrap();
    let weaver = weaver_dir(dir.path());
    fs::create_dir_all(&weaver).unwrap();

    assert_eq!(
        auto_weave_gate(&weaver, dir.path(), false, false).unwrap(),
        WeaveGate::Disabled
    );

    // Fresh marker → too soon even when enabled.
    fs::write(weaver.join(LAST_WEAVE_FILENAME), b"").unwrap();
    matches!(
        auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
        WeaveGate::TooSoon { .. }
    );

    // Force bypasses everything but the lock.
    assert_eq!(
        auto_weave_gate(&weaver, dir.path(), true, true).unwrap(),
        WeaveGate::Ready
    );
}

#[test]
fn auto_weave_gate_requires_enough_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let weaver = weaver_dir(dir.path());
    fs::create_dir_all(&weaver).unwrap();
    // Enabled, no marker, but 0 sessions → TooFewSessions.
    matches!(
        auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
        WeaveGate::TooFewSessions { touched: 0, required: 3 }
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claw-runtime auto_weave_gate`
Expected: FAIL — gate not found.

- [ ] **Step 3: Implement gate, config, wiring**

Gate (in `skill_weaver.rs`) — port `auto_dream_gate` (`dreamer.rs:471`) with weaver names; session counting: reuse `dreamer.rs`'s `touched_sessions_since` if visible, otherwise count `*.jsonl` files in `workspace_sessions_dir(cwd)` modified after the marker:

```rust
pub const AUTO_WEAVE_MIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const AUTO_WEAVE_MIN_TOUCHED_SESSIONS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeaveGate {
    Disabled,
    Locked,
    TooSoon { remaining: Duration },
    TooFewSessions { touched: usize, required: usize },
    Ready,
}

pub fn auto_weave_gate(
    weaver: &Path,
    cwd: &Path,
    enabled: bool,
    force: bool,
) -> Result<WeaveGate, WeaverError> {
    if !enabled && !force {
        return Ok(WeaveGate::Disabled);
    }
    if weaver.join(WEAVE_LOCK_FILENAME).exists() {
        return Ok(WeaveGate::Locked);
    }
    if force {
        return Ok(WeaveGate::Ready);
    }
    if let Some(last) = last_weave_time(weaver) {
        let elapsed = SystemTime::now().duration_since(last).unwrap_or(Duration::ZERO);
        if elapsed < AUTO_WEAVE_MIN_INTERVAL {
            return Ok(WeaveGate::TooSoon {
                remaining: AUTO_WEAVE_MIN_INTERVAL - elapsed,
            });
        }
    }
    let touched = sessions_touched_since(cwd, last_weave_time(weaver))?;
    if touched < AUTO_WEAVE_MIN_TOUCHED_SESSIONS {
        return Ok(WeaveGate::TooFewSessions {
            touched,
            required: AUTO_WEAVE_MIN_TOUCHED_SESSIONS,
        });
    }
    Ok(WeaveGate::Ready)
}

pub fn maybe_run_auto_weave(
    cwd: &Path,
    config: &WeaverConfig,
    client: &mut impl ApiClient,
) -> Result<Option<WeaveRun>, WeaverError> {
    let weaver = weaver_dir(cwd);
    if auto_weave_gate(&weaver, cwd, config.auto_weave, false)? != WeaveGate::Ready {
        return Ok(None);
    }
    run_weave_pass(cwd, client, config.max_input_bytes).map(Some)
}
```

`WeaverConfig` in `config.rs` — copy `MemoryConfig`'s exact derive stack and default-fn idiom:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WeaverConfig {
    #[serde(default)]
    pub auto_weave: bool,
    #[serde(default = "default_weaver_max_input_bytes")]
    pub max_input_bytes: usize,
}

fn default_weaver_max_input_bytes() -> usize {
    64 * 1024
}

impl Default for WeaverConfig {
    fn default() -> Self {
        WeaverConfig {
            auto_weave: false,
            max_input_bytes: default_weaver_max_input_bytes(),
        }
    }
}
```

Add the field to the top-level settings struct exactly where `MemoryConfig` sits, with `#[serde(default)]`. In `main.rs`, immediately after the existing `maybe_run_auto_dream` call site, add `maybe_run_auto_weave(&cwd, &settings.weaver, &mut client)` with the same error-swallowing/logging treatment auto-dream gets.

USAGE.md: add a "Learned skills (Skill Weaver)" section documenting the loop, the five `/skills` verbs, `.claw/skills/learned/`, quarantine semantics, and the `weaver.auto_weave` setting (default off).

- [ ] **Step 4: Run full verification**

Run (from `rust/`): `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`, then from repo root: `scripts/fmt.sh --check`.
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs rust/crates/runtime/src/config.rs rust/crates/rusty-claude-cli/src/main.rs USAGE.md
git commit -m "feat: auto-weave gate, weaver config, and usage docs"
```

---

## Deferred (explicitly out of scope — YAGNI until the loop above proves out)

- **Automatic outcome attribution** (inferring skill success/failure from turn results instead of `/skills mark`): needs a reliable turn-level success signal; revisit once `workflow_gates.rs`/`green_contract.rs` expose one.
- **Skill practice/rehearsal** (SkillWeaver's active honing — running a skill in a sandbox to test it): the sandbox (`sandbox.rs`) makes this feasible later; passive honing via real usage first.
- **Skill transfer/export bundles** (`skills export`/`import` — the paper's strong→weak agent transfer): plain directory copy of `.claw/skills/learned/` already works; add tooling only if asked.
- **Provider-driven skill refinement** (rewrite a quarantined skill instead of parking it): add as a `weave --refine` flag once quarantine data exists.

## Research references

- SkillWeaver paper: [arXiv:2504.07079](https://arxiv.org/abs/2504.07079) — propose/synthesize/hone loop, skill library, transfer results (+31.8% WebArena, +54.3% weak-agent transfer).
- HF paper page: [huggingface.co/papers/2504.07079](https://huggingface.co/papers/2504.07079)
- In-repo prior art: `rust/crates/runtime/src/dreamer.rs` (provider pass over local logs with locks/gates — the template this plan follows), `rust/crates/runtime/src/harness_assets.rs` (skill discovery contract), `~/.claude/skills/omc-learned` lookup root in `rust/crates/commands/src/lib.rs:3848` (existing learned-skills precedent).
