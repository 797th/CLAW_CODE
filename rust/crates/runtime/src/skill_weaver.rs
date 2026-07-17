//! SkillWeaver-style self-improvement: distill session trajectories into
//! learned skills, track per-skill outcomes, refine or quarantine failures.
//! Modeled on dreamer.rs (locks, gates, atomic writes, provider pass).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::WeaverConfig;
use crate::conversation::{ApiClient, ApiRequest, AssistantEvent};
use crate::harness_assets::parse_frontmatter_value;
use crate::session::{ContentBlock, ConversationMessage, MessageRole};

pub const WEAVER_DIR_NAME: &str = ".weaver";
pub const STATS_FILENAME: &str = "stats.json";
pub const LAST_WEAVE_FILENAME: &str = ".last-weave";
pub const WEAVE_LOCK_FILENAME: &str = ".weave-lock";
pub const LEARNED_DIR_NAME: &str = "learned";
const MAX_SKILLS_PER_PASS: usize = 3;

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

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

/// `<cwd>/.claw/skills/.weaver`
pub fn weaver_dir(cwd: &Path) -> PathBuf {
    cwd.join(".claw").join("skills").join(WEAVER_DIR_NAME)
}

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
    files.sort_by_key(|a| std::cmp::Reverse(a.1));

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

// ---------------------------------------------------------------------------
// Weave pass: synthesize + write learned skills
// ---------------------------------------------------------------------------

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

/// Public validity check for skill names, using the same convention
/// `quarantine_skill`/`restore_skill` enforce. Callers outside this module
/// (e.g. `commands::run_skills_mark`) should validate through this rather
/// than reimplementing the kebab-case/length rule.
#[must_use]
pub fn is_valid_skill_name(name: &str) -> bool {
    valid_skill_name(name)
}

fn collect_text_from_events(events: &[AssistantEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if let AssistantEvent::TextDelta(delta) = event {
            text.push_str(delta);
        }
    }
    text.trim().to_string()
}

/// Parse `<skill name="...">...</skill>` blocks out of the raw provider text.
fn parse_weaver_output(raw: &str) -> Result<Vec<WovenSkill>, WeaverError> {
    let mut skills = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rest = raw;
    while let Some(start) = rest.find("<skill name=\"") {
        let after = &rest[start + "<skill name=\"".len()..];
        let Some(name_end) = after.find('"') else {
            break;
        };
        let name = after[..name_end].to_string();
        let Some(body_start) = after.find('>') else {
            break;
        };
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
        if !seen_names.insert(name.clone()) {
            return Err(WeaverError::InvalidOutput(format!(
                "duplicate skill name '{name}' in one response"
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
    let raw = collect_text_from_events(&events);
    parse_weaver_output(&raw)
}

pub fn write_learned_skills(
    skills: &[WovenSkill],
    skills_root: &Path,
) -> Result<Vec<PathBuf>, WeaverError> {
    // Two-phase write: stage every skill's tmp file first; only rename any
    // of them into place once every tmp write in the batch has succeeded.
    // This keeps a failure partway through a multi-skill pass from leaving
    // an earlier skill's SKILL.md live while the pass as a whole reports
    // Err (mirrors dreamer.rs's atomic_write discipline, extended to a
    // whole-batch guarantee).
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new(); // (tmp, dest)

    let stage_result = (|| -> Result<(), WeaverError> {
        for skill in skills {
            if !valid_skill_name(&skill.name) {
                return Err(WeaverError::InvalidOutput(format!(
                    "invalid skill name '{}'",
                    skill.name
                )));
            }
            let dir = skills_root.join(LEARNED_DIR_NAME).join(&skill.name);
            fs::create_dir_all(&dir)?;
            let dest = dir.join("SKILL.md");
            let tmp = dir.join("SKILL.md.tmp");
            let mut body = skill.markdown.clone();
            if !body.ends_with('\n') {
                body.push('\n');
            }
            fs::write(&tmp, body)?;
            staged.push((tmp, dest));
        }
        Ok(())
    })();

    if let Err(err) = stage_result {
        // Clean up any tmp files already written before returning the error.
        for (tmp, _dest) in &staged {
            let _ = fs::remove_file(tmp);
        }
        return Err(err);
    }

    let mut written = Vec::new();
    for (tmp, dest) in staged {
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
    let _lock = WeaveLock::try_acquire(&weaver)?;

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

// ---------------------------------------------------------------------------
// Auto-weave gate
// ---------------------------------------------------------------------------

/// Minimum time between automatic weave passes for a given workspace.
pub const AUTO_WEAVE_MIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Minimum number of sessions touched since the last weave before an
/// automatic pass is allowed to run.
pub const AUTO_WEAVE_MIN_TOUCHED_SESSIONS: usize = 3;

/// Auto-weave gate decision. Mirrors `dreamer::DreamGate` in shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeaveGate {
    Disabled,
    Locked,
    TooSoon { remaining: Duration },
    TooFewSessions { touched: usize, required: usize },
    Ready,
}

/// Count `*.jsonl` session files in `workspace_sessions_dir(cwd)` modified
/// after `since` (or all of them, when `since` is `None`). Local counterpart
/// to `dreamer::touched_sessions_since`, which is private to that module.
fn sessions_touched_since(cwd: &Path, since: Option<SystemTime>) -> Result<usize, WeaverError> {
    let sessions_dir = crate::session::workspace_sessions_dir(cwd)
        .map_err(|e| WeaverError::Io(io::Error::other(e.to_string())))?;
    let entries = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(0),
    };
    let mut touched = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if since.is_none_or(|cutoff| modified > cutoff) {
            touched += 1;
        }
    }
    Ok(touched)
}

/// Check auto-weave gates for the current workspace.
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
        let elapsed = SystemTime::now()
            .duration_since(last)
            .unwrap_or(Duration::ZERO);
        if elapsed < AUTO_WEAVE_MIN_INTERVAL {
            return Ok(WeaveGate::TooSoon {
                remaining: AUTO_WEAVE_MIN_INTERVAL
                    .checked_sub(elapsed)
                    .unwrap_or(Duration::ZERO),
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

/// Run an auto-weave pass only when gates allow it.
pub fn maybe_run_auto_weave(
    cwd: &Path,
    config: &WeaverConfig,
    client: &mut impl ApiClient,
) -> Result<Option<WeaveRun>, WeaverError> {
    let weaver = weaver_dir(cwd);
    if auto_weave_gate(&weaver, cwd, config.auto_weave(), false)? != WeaveGate::Ready {
        return Ok(None);
    }
    run_weave_pass(cwd, client, config.max_input_bytes()).map(Some)
}

struct WeaveLock {
    path: PathBuf,
}

impl WeaveLock {
    fn try_acquire(weaver_dir: &Path) -> Result<Self, WeaverError> {
        fs::create_dir_all(weaver_dir)?;
        let path = weaver_dir.join(WEAVE_LOCK_FILENAME);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                writeln!(file, "pid={}", std::process::id())?;
                Ok(Self { path })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(WeaverError::Locked),
            Err(error) => Err(WeaverError::Io(error)),
        }
    }
}

impl Drop for WeaveLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Honing pass: quarantine failing learned skills
// ---------------------------------------------------------------------------

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

/// Whether `name` currently has a quarantined (parked) learned-skill file,
/// i.e. `learned/<name>/SKILL.md.quarantined` exists. Promoted out of
/// `commands` so the `learned/<name>/...quarantined` path convention lives
/// in one place instead of being duplicated at call sites.
#[must_use]
pub fn is_quarantined(cwd: &Path, name: &str) -> bool {
    let Some(live) = learned_skill_file(cwd, name) else {
        return false;
    };
    live.with_extension(format!("md.{QUARANTINE_SUFFIX}"))
        .is_file()
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

    const VALID_SKILL_BLOCK: &str = r#"<skill name="fix-clippy-warnings">
---
name: fix-clippy-warnings
description: Run clippy with -D warnings and fix each lint mechanically before committing
---

# Fix clippy warnings

1. Run `cargo clippy --workspace --all-targets -- -D warnings`.
2. Fix each warning in source order; re-run until clean.
</skill>"#;

    #[test]
    fn parse_weaver_output_parses_valid_skill_block() {
        let skills = parse_weaver_output(VALID_SKILL_BLOCK).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "fix-clippy-warnings");
        assert!(skills[0].markdown.starts_with("---"));
    }

    #[test]
    fn parse_weaver_output_empty_response_is_empty_skills() {
        let skills = parse_weaver_output("").unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn parse_weaver_output_rejects_path_escaping_name() {
        let bad = r#"<skill name="../evil">
---
name: ../evil
description: nope
---
body
</skill>"#;
        let err = parse_weaver_output(bad).unwrap_err();
        assert!(matches!(err, WeaverError::InvalidOutput(_)));
    }

    #[test]
    fn parse_weaver_output_rejects_frontmatter_name_mismatch() {
        let bad = r#"<skill name="fix-clippy-warnings">
---
name: different-name
description: nope
---
body
</skill>"#;
        let err = parse_weaver_output(bad).unwrap_err();
        assert!(matches!(err, WeaverError::InvalidOutput(_)));
    }

    #[test]
    fn parse_weaver_output_rejects_missing_description() {
        let bad = r#"<skill name="fix-clippy-warnings">
---
name: fix-clippy-warnings
---
body
</skill>"#;
        let err = parse_weaver_output(bad).unwrap_err();
        assert!(matches!(err, WeaverError::InvalidOutput(_)));
    }

    #[test]
    fn parse_weaver_output_truncates_to_max_skills_per_pass() {
        let mut raw = String::new();
        for i in 0..5 {
            raw.push_str(&format!(
                "<skill name=\"skill-number-{i}\">\n---\nname: skill-number-{i}\ndescription: skill {i}\n---\nbody\n</skill>\n"
            ));
        }
        let skills = parse_weaver_output(&raw).unwrap();
        assert_eq!(skills.len(), MAX_SKILLS_PER_PASS);
    }

    #[test]
    fn write_learned_skills_rejects_path_escaping_name_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let skills = vec![WovenSkill {
            name: "../evil".to_string(),
            markdown: "---\nname: ../evil\ndescription: nope\n---\nbody\n".to_string(),
        }];
        let err = write_learned_skills(&skills, dir.path()).unwrap_err();
        assert!(matches!(err, WeaverError::InvalidOutput(_)));
        assert!(!dir.path().join("learned").exists());
    }

    #[test]
    fn parse_weaver_output_rejects_duplicate_skill_names() {
        let dup = r#"<skill name="foo-bar-baz">
---
name: foo-bar-baz
description: first
---
body one
</skill>
<skill name="foo-bar-baz">
---
name: foo-bar-baz
description: second
---
body two
</skill>"#;
        let err = parse_weaver_output(dup).unwrap_err();
        match err {
            WeaverError::InvalidOutput(msg) => assert!(msg.contains("foo-bar-baz")),
            other => panic!("expected InvalidOutput, got {other:?}"),
        }
    }

    #[test]
    fn write_learned_skills_is_all_or_nothing_on_mid_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let skills_root = dir.path();
        // Pre-create `learned/skill-two` as a plain FILE so create_dir_all/write
        // for the second skill fails partway through the batch.
        let learned = skills_root.join(LEARNED_DIR_NAME);
        fs::create_dir_all(&learned).unwrap();
        fs::write(learned.join("skill-two"), b"not a directory").unwrap();

        let skills = vec![
            WovenSkill {
                name: "skill-one".to_string(),
                markdown: "---\nname: skill-one\ndescription: first\n---\nbody\n".to_string(),
            },
            WovenSkill {
                name: "skill-two".to_string(),
                markdown: "---\nname: skill-two\ndescription: second\n---\nbody\n".to_string(),
            },
        ];

        let err = write_learned_skills(&skills, skills_root).unwrap_err();
        assert!(matches!(err, WeaverError::Io(_)));

        // Skill one must NOT have been left live: no live SKILL.md, only
        // (at most) a cleaned-up tmp state.
        assert!(!learned.join("skill-one").join("SKILL.md").is_file());
    }

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
        assert!(matches!(
            auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
            WeaveGate::TooSoon { .. }
        ));

        // Force bypasses everything but the lock.
        assert_eq!(
            auto_weave_gate(&weaver, dir.path(), true, true).unwrap(),
            WeaveGate::Ready
        );
    }

    #[test]
    fn auto_weave_gate_locked_when_lock_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        fs::create_dir_all(&weaver).unwrap();
        fs::write(weaver.join(WEAVE_LOCK_FILENAME), b"pid=1").unwrap();

        assert_eq!(
            auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
            WeaveGate::Locked
        );
        // Force still respects the lock.
        assert_eq!(
            auto_weave_gate(&weaver, dir.path(), true, true).unwrap(),
            WeaveGate::Locked
        );
    }

    #[test]
    fn auto_weave_gate_requires_enough_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        fs::create_dir_all(&weaver).unwrap();
        // Enabled, no marker, but 0 sessions → TooFewSessions.
        assert!(matches!(
            auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
            WeaveGate::TooFewSessions {
                touched: 0,
                required: 3
            }
        ));

        let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        for i in 0..3 {
            fs::write(sessions.join(format!("s{i}.jsonl")), "session\n").unwrap();
        }
        assert_eq!(
            auto_weave_gate(&weaver, dir.path(), true, false).unwrap(),
            WeaveGate::Ready
        );
    }

    #[test]
    fn is_quarantined_reflects_parked_state() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".claw/skills/learned/flaky-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: flaky-skill\ndescription: test\n---\nbody\n",
        )
        .unwrap();

        assert!(!is_quarantined(dir.path(), "flaky-skill"));
        quarantine_skill(dir.path(), "flaky-skill").unwrap();
        assert!(is_quarantined(dir.path(), "flaky-skill"));
        restore_skill(dir.path(), "flaky-skill").unwrap();
        assert!(!is_quarantined(dir.path(), "flaky-skill"));
    }

    #[test]
    fn is_valid_skill_name_matches_convention() {
        assert!(is_valid_skill_name("fix-clippy-warnings"));
        assert!(!is_valid_skill_name("../evil"));
        assert!(!is_valid_skill_name("ab"));
    }
}
