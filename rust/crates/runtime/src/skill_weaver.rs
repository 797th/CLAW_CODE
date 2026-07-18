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

// ---------------------------------------------------------------------------
// Attribution windows: pending skill invocations awaiting a turn outcome
// ---------------------------------------------------------------------------

pub const PENDING_FILENAME: &str = "pending.json";
/// Pending invocations older than this are discarded on drain, never judged
/// (protects against crashed processes and long-idle sessions).
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// One Skill-tool invocation awaiting outcome attribution at turn end.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingInvocation {
    pub skill: String,
    pub opened_ms: u64,
}

fn load_pending(weaver_dir: &Path) -> Vec<PendingInvocation> {
    match fs::read_to_string(weaver_dir.join(PENDING_FILENAME)) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_pending(weaver_dir: &Path, pending: &[PendingInvocation]) -> io::Result<()> {
    let dest = weaver_dir.join(PENDING_FILENAME);
    if pending.is_empty() {
        match fs::remove_file(&dest) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }

    fs::create_dir_all(weaver_dir)?;
    let tmp = weaver_dir.join(format!("{PENDING_FILENAME}.tmp"));
    let raw = serde_json::to_string_pretty(pending)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, &dest)?;
    Ok(())
}

/// Record that `skill` was just invoked and is awaiting an outcome.
/// Advisory: failures are swallowed; blank names are ignored. Accepts any
/// non-empty name (plugin-prefixed hand-written skills included) — the
/// learned-skill kebab rule only gates file writes, not telemetry.
pub fn open_attribution_window(weaver_dir: &Path, skill: &str) {
    let skill = skill.trim();
    if skill.is_empty() {
        return;
    }

    let mut pending = load_pending(weaver_dir);
    pending.push(PendingInvocation {
        skill: skill.to_string(),
        opened_ms: now_ms(),
    });
    let _ = save_pending(weaver_dir, &pending);
}

/// Drop any pending windows for `skill` (used when `/skills mark` supplies
/// an explicit verdict, which overrides heuristic attribution).
pub fn clear_pending_for(weaver_dir: &Path, skill: &str) {
    let mut pending = load_pending(weaver_dir);
    let before = pending.len();
    pending.retain(|entry| entry.skill != skill);
    if pending.len() != before {
        let _ = save_pending(weaver_dir, &pending);
    }
}

/// Remove and return all pending windows, discarding stale entries.
/// Stale windows are never judged — a crashed process must not count as a
/// skill failure.
pub fn drain_pending(weaver_dir: &Path) -> Vec<PendingInvocation> {
    let pending = load_pending(weaver_dir);
    let _ = fs::remove_file(weaver_dir.join(PENDING_FILENAME));
    let now = now_ms();
    let max_age_ms = PENDING_MAX_AGE.as_millis() as u64;
    pending
        .into_iter()
        .filter(|entry| now.saturating_sub(entry.opened_ms) <= max_age_ms)
        .collect()
}

/// Classify a completed turn into a skill outcome using advisory heuristics.
/// Terminal turn failures always count as `Failure`; otherwise, a clean turn
/// is `Success`, one tool error is ambiguous (`None`), and two or more tool
/// errors count as `Failure` because the turn likely cascaded.
#[must_use]
pub fn classify_turn_outcome(turn_failed: bool, tool_error_count: usize) -> Option<SkillOutcome> {
    if turn_failed {
        return Some(SkillOutcome::Failure);
    }

    match tool_error_count {
        0 => Some(SkillOutcome::Success),
        1 => None,
        _ => Some(SkillOutcome::Failure),
    }
}

/// Count failing tool-result blocks captured in a completed runtime turn.
#[must_use]
pub fn turn_tool_error_count(summary: &crate::conversation::TurnSummary) -> usize {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
        .count()
}

/// Drain pending attribution windows, judging only those opened during the
/// current turn. Older windows are discarded without verdict so a later turn
/// cannot retroactively judge them.
pub fn settle_attribution_windows(
    cwd: &Path,
    outcome: Option<SkillOutcome>,
    opened_after_ms: u64,
) -> Vec<String> {
    let weaver = weaver_dir(cwd);
    let pending = drain_pending(&weaver);
    let current_turn: Vec<PendingInvocation> = pending
        .into_iter()
        .filter(|entry| entry.opened_ms >= opened_after_ms)
        .collect();
    let settled: Vec<String> = current_turn
        .iter()
        .map(|entry| entry.skill.clone())
        .collect();

    if let Some(outcome) = outcome {
        if !current_turn.is_empty() {
            let mut ledger = SkillLedger::load(&weaver);
            for entry in &current_turn {
                ledger.record(&entry.skill, outcome);
            }
            let _ = ledger.save(&weaver);
        }
    }

    settled
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

/// Per-line payload cap in condensed trajectories. Big enough to keep exact
/// commands, small enough that tool payloads cannot eat the episode budget.
const CONDENSED_SNIPPET_CHARS: usize = 200;

fn push_condensed_line(out: &mut String, label: &str, body: &str) {
    let flattened = body.replace('\n', " ");
    let snippet: String = flattened.chars().take(CONDENSED_SNIPPET_CHARS).collect();
    let ellipsis = if flattened.chars().count() > CONDENSED_SNIPPET_CHARS {
        "…"
    } else {
        ""
    };
    out.push_str(label);
    out.push_str(": ");
    out.push_str(&snippet);
    out.push_str(ellipsis);
    out.push('\n');
}

/// Condense a parseable session file into a tool-call trajectory. Returns
/// `None` when the file does not parse as a session or carries no recipe
/// signal, letting callers fall back to the raw file content.
fn condense_session_content(path: &Path) -> Option<String> {
    let session = crate::session::Session::load_from_path(path).ok()?;
    if session.messages.is_empty() {
        return None;
    }

    let mut out = String::new();
    for message in &session.messages {
        for block in &message.blocks {
            match (message.role, block) {
                (MessageRole::User, ContentBlock::Text { text }) => {
                    push_condensed_line(&mut out, "user", text);
                }
                (MessageRole::Assistant, ContentBlock::Text { text }) => {
                    push_condensed_line(&mut out, "assistant", text);
                }
                (MessageRole::Assistant, ContentBlock::ToolUse { name, input, .. }) => {
                    push_condensed_line(&mut out, &format!("tool {name}"), input);
                }
                (
                    _,
                    ContentBlock::ToolResult {
                        tool_name,
                        output,
                        is_error,
                        ..
                    },
                ) => {
                    let status = if *is_error { "error" } else { "ok" };
                    push_condensed_line(&mut out, &format!("result {tool_name} {status}"), output);
                }
                _ => {}
            }
        }
    }

    if out.is_empty() {
        return None;
    }
    Some(out)
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
        let content = match condense_session_content(&path) {
            Some(condensed) => condensed,
            None => {
                let Ok(raw) = fs::read_to_string(&path) else {
                    continue;
                };
                raw
            }
        };
        if content.len() > budget {
            // This file alone would blow the remaining budget. Skip it and
            // keep scanning older files rather than aborting the whole scan
            // — otherwise a single oversize newest session starves weaving
            // forever (`.last-weave` never advances since callers treat an
            // empty result as `NoEpisodes` and bail before writing it).
            continue;
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

/// Whether `skills_root/learned/<name>` currently has a quarantined
/// (parked) `SKILL.md.quarantined` file. Structural check only — mirrors
/// the guard `hone()` uses, but keyed off `skills_root` (`.claw/skills`)
/// rather than `cwd`, since that's what `write_learned_skills` already has
/// in hand.
fn learned_skill_is_quarantined(skills_root: &Path, name: &str) -> bool {
    skills_root
        .join(LEARNED_DIR_NAME)
        .join(name)
        .join(format!("SKILL.md.{QUARANTINE_SUFFIX}"))
        .is_file()
}

/// Names of learned skills currently quarantined under
/// `cwd/.claw/skills/learned/<name>/SKILL.md.quarantined`.
///
/// `harness_assets::discover` only sees live `SKILL.md` files, so a
/// quarantined name silently drops off its "existing skills" list — the
/// weaver provider would then be free to re-propose the same name, and
/// (without the write-boundary guard in `write_learned_skills`) resurrect
/// it as a live skill sitting right next to its own quarantine record.
/// Call sites fold this into the "do not duplicate" hint alongside
/// `discover(cwd)`'s names.
#[must_use]
pub fn quarantined_learned_skill_names(cwd: &Path) -> Vec<String> {
    let learned_root = cwd.join(".claw").join("skills").join(LEARNED_DIR_NAME);
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(&learned_root) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(format!("SKILL.md.{QUARANTINE_SUFFIX}")).is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            names.push(name.to_string());
        }
    }
    names
}

pub fn write_learned_skills(
    skills: &[WovenSkill],
    skills_root: &Path,
) -> Result<(Vec<PathBuf>, Vec<String>), WeaverError> {
    // Two-phase write: stage every skill's tmp file first; only rename any
    // of them into place once every tmp write in the batch has succeeded.
    // This keeps a failure partway through a multi-skill pass from leaving
    // an earlier skill's SKILL.md live while the pass as a whole reports
    // Err (mirrors dreamer.rs's atomic_write discipline, extended to a
    // whole-batch guarantee).
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new(); // (tmp, dest)
    let mut skipped_quarantined: Vec<String> = Vec::new();

    let stage_result = (|| -> Result<(), WeaverError> {
        for skill in skills {
            if !valid_skill_name(&skill.name) {
                return Err(WeaverError::InvalidOutput(format!(
                    "invalid skill name '{}'",
                    skill.name
                )));
            }
            // Write-boundary guard for I4: the "do not duplicate" hint
            // handed to the provider can still be ignored (or racing with
            // a hone pass), so re-check here before ever touching disk.
            // Skipping (not failing the whole batch) matches how the rest
            // of this function treats a single bad skill as a fatal error
            // only when it would silently overwrite/corrupt state — here
            // nothing is corrupted by leaving the quarantined file alone,
            // so we drop this one skill and keep the rest of the pass.
            if learned_skill_is_quarantined(skills_root, &skill.name) {
                skipped_quarantined.push(skill.name.clone());
                continue;
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
    Ok((written, skipped_quarantined))
}

#[derive(Debug)]
pub struct WeaveRun {
    pub files_written: Vec<PathBuf>,
    pub episode_count: usize,
    /// Skill names the provider re-proposed that are currently quarantined
    /// (`learned/<name>/SKILL.md.quarantined`); skipped rather than
    /// resurrected as a live `SKILL.md`. See I4.
    pub skipped_quarantined: Vec<String>,
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
    let mut existing: Vec<String> = crate::harness_assets::discover(cwd)
        .skills
        .into_iter()
        .map(|s| s.name)
        .collect();
    // Quarantined learned skills drop out of `discover()` (it only sees
    // live SKILL.md files), so fold them back into the "do not duplicate"
    // hint explicitly — otherwise the provider is free to re-propose a
    // name that's already parked. write_learned_skills enforces this at
    // the write boundary too, in case the hint is ignored.
    existing.extend(quarantined_learned_skill_names(cwd));
    let skills = synthesize_skills(&episodes, &existing, client)?;
    let skills_root = cwd.join(".claw").join("skills");
    let (files_written, skipped_quarantined) = write_learned_skills(&skills, &skills_root)?;
    fs::write(weaver.join(LAST_WEAVE_FILENAME), b"")?;
    Ok(WeaveRun {
        files_written,
        episode_count: episodes.len(),
        skipped_quarantined,
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
    fn pending_windows_open_drain_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());

        assert!(drain_pending(&weaver).is_empty());

        open_attribution_window(&weaver, "fix-clippy-warnings");
        open_attribution_window(&weaver, "superpowers:brainstorming");
        open_attribution_window(&weaver, "   ");

        let drained = drain_pending(&weaver);
        let names: Vec<&str> = drained
            .iter()
            .map(|pending| pending.skill.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["fix-clippy-warnings", "superpowers:brainstorming"]
        );
        assert!(drained.iter().all(|pending| pending.opened_ms > 0));
        assert!(!weaver.join(PENDING_FILENAME).exists());
        assert!(drain_pending(&weaver).is_empty());
    }

    #[test]
    fn clear_pending_for_removes_only_that_skill() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        open_attribution_window(&weaver, "skill-alpha");
        open_attribution_window(&weaver, "skill-beta");

        clear_pending_for(&weaver, "skill-alpha");

        let drained = drain_pending(&weaver);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].skill, "skill-beta");
    }

    #[test]
    fn drain_pending_discards_stale_entries() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        fs::create_dir_all(&weaver).unwrap();

        let fresh = now_ms();
        let raw = format!(
            r#"[{{"skill":"ancient-skill","opened_ms":1}},{{"skill":"fresh-skill","opened_ms":{fresh}}}]"#
        );
        fs::write(weaver.join(PENDING_FILENAME), raw).unwrap();

        let drained = drain_pending(&weaver);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].skill, "fresh-skill");
    }

    #[test]
    fn pending_load_missing_or_corrupt_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        fs::create_dir_all(&weaver).unwrap();
        fs::write(weaver.join(PENDING_FILENAME), "{not json").unwrap();
        assert!(drain_pending(&weaver).is_empty());
    }

    fn turn_summary_with_tool_errors(tool_errors: &[bool]) -> crate::conversation::TurnSummary {
        crate::conversation::TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: tool_errors
                .iter()
                .enumerate()
                .map(|(index, is_error)| {
                    crate::session::ConversationMessage::tool_result(
                        format!("tool-use-{index}"),
                        format!("tool-{index}"),
                        format!("output-{index}"),
                        *is_error,
                    )
                })
                .collect(),
            prompt_cache_events: Vec::new(),
            iterations: 1,
            usage: crate::usage::TokenUsage::default(),
            auto_compaction: None,
            lifecycle_warnings: Vec::new(),
            gate_events: Vec::new(),
        }
    }

    #[test]
    fn classify_turn_outcome_policy() {
        assert_eq!(classify_turn_outcome(true, 0), Some(SkillOutcome::Failure));
        assert_eq!(classify_turn_outcome(false, 0), Some(SkillOutcome::Success));
        assert_eq!(classify_turn_outcome(false, 1), None);
        assert_eq!(classify_turn_outcome(false, 2), Some(SkillOutcome::Failure));
    }

    #[test]
    fn turn_tool_error_count_counts_only_error_results() {
        let summary = turn_summary_with_tool_errors(&[false, true, true, false]);
        assert_eq!(turn_tool_error_count(&summary), 2);
    }

    #[test]
    fn settle_records_outcome_for_this_turns_windows_only() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        open_attribution_window(&weaver, "old-turn-skill");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let turn_start = now_ms();
        open_attribution_window(&weaver, "this-turn-skill");

        let settled =
            settle_attribution_windows(dir.path(), Some(SkillOutcome::Success), turn_start);
        assert_eq!(settled, vec!["this-turn-skill".to_string()]);

        let ledger = SkillLedger::load(&weaver);
        assert_eq!(ledger.entry("this-turn-skill").unwrap().successes, 1);
        assert!(ledger.entry("old-turn-skill").is_none());
        assert!(settle_attribution_windows(dir.path(), Some(SkillOutcome::Success), 0).is_empty());
    }

    #[test]
    fn settle_ambiguous_turn_drains_without_recording() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        let turn_start = now_ms();
        open_attribution_window(&weaver, "ambiguous-skill");

        let settled = settle_attribution_windows(dir.path(), None, turn_start);
        assert_eq!(settled, vec!["ambiguous-skill".to_string()]);
        assert!(SkillLedger::load(&weaver)
            .entry("ambiguous-skill")
            .is_none());
        assert!(drain_pending(&weaver).is_empty());
    }

    #[test]
    fn collect_episodes_condenses_parseable_sessions_to_tool_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
        fs::create_dir_all(&sessions).unwrap();

        let mut session = crate::session::Session::new();
        session
            .push_message(ConversationMessage {
                role: MessageRole::User,
                blocks: vec![ContentBlock::Text {
                    text: "Fix the clippy warnings in the runtime crate".to_string(),
                }],
                usage: None,
            })
            .unwrap();
        session
            .push_message(ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::ToolUse {
                    id: "tu1".to_string(),
                    name: "bash".to_string(),
                    input: format!(
                        "{{\"command\":\"cargo clippy\",\"padding\":\"{}\"}}",
                        "x".repeat(2000)
                    ),
                }],
                usage: None,
            })
            .unwrap();
        session
            .push_message(ConversationMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu1".to_string(),
                    tool_name: "bash".to_string(),
                    output: format!("warning: unused import{}", "y".repeat(5000)),
                    is_error: false,
                }],
                usage: None,
            })
            .unwrap();
        let path = sessions.join("condensable.jsonl");
        session.save_to_path(&path).unwrap();
        let raw_len = fs::read_to_string(&path).unwrap().len();

        let episodes = collect_episodes(dir.path(), None, 64 * 1024).unwrap();
        assert_eq!(episodes.len(), 1);
        let content = &episodes[0].content;
        assert!(content.contains("user: Fix the clippy warnings"));
        assert!(content.contains("tool bash:"));
        assert!(content.contains("result bash ok:"));
        assert!(
            content.len() < raw_len / 2,
            "condensed {} vs raw {}",
            content.len(),
            raw_len
        );
    }

    #[test]
    fn collect_episodes_falls_back_to_raw_for_unparseable_files() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("not-a-session.jsonl"), "plain old text\n").unwrap();

        let episodes = collect_episodes(dir.path(), None, 1024).unwrap();
        assert_eq!(episodes.len(), 1);
        assert!(episodes[0].content.contains("plain old text"));
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
    fn collect_episodes_skips_oversize_newest_file_and_keeps_older_ones() {
        // Regression for I2: an oversize *newest* session must not zero out
        // the whole result. The scan should skip it and keep filling the
        // budget from older, smaller files instead of bailing out early.
        let dir = tempfile::tempdir().unwrap();
        let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let small = "old small session\n";
        fs::write(sessions.join("a-old.jsonl"), small).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let oversize = "x".repeat(small.len() + 1);
        fs::write(sessions.join("b-new.jsonl"), &oversize).unwrap();

        // Budget large enough for the small file but not the oversize newest one.
        let episodes = collect_episodes(dir.path(), None, small.len()).unwrap();
        assert_eq!(episodes.len(), 1);
        assert!(episodes[0].content.contains("old small session"));
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
    fn write_learned_skills_skips_quarantined_name_without_resurrecting_it() {
        // Regression for I4: re-proposing a quarantined skill's name must
        // not write a live SKILL.md next to the SKILL.md.quarantined park
        // file. The write boundary is the last line of defense even if the
        // "do not duplicate" hint sent to the provider was ignored.
        let dir = tempfile::tempdir().unwrap();
        let skills_root = dir.path();
        let learned = skills_root.join(LEARNED_DIR_NAME).join("flaky-skill");
        fs::create_dir_all(&learned).unwrap();
        fs::write(
            learned.join("SKILL.md.quarantined"),
            "---\nname: flaky-skill\ndescription: test\n---\nbody\n",
        )
        .unwrap();

        let skills = vec![WovenSkill {
            name: "flaky-skill".to_string(),
            markdown: "---\nname: flaky-skill\ndescription: reproposed\n---\nnew body\n"
                .to_string(),
        }];
        let (written, skipped) = write_learned_skills(&skills, skills_root).unwrap();
        assert!(written.is_empty());
        assert_eq!(skipped, vec!["flaky-skill".to_string()]);
        assert!(!learned.join("SKILL.md").exists());
        // Parked file must be untouched.
        assert_eq!(
            fs::read_to_string(learned.join("SKILL.md.quarantined")).unwrap(),
            "---\nname: flaky-skill\ndescription: test\n---\nbody\n"
        );
    }

    #[test]
    fn quarantined_learned_skill_names_lists_parked_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".claw/skills/learned/flaky-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: flaky-skill\ndescription: test\n---\nbody\n",
        )
        .unwrap();
        assert!(quarantined_learned_skill_names(dir.path()).is_empty());

        quarantine_skill(dir.path(), "flaky-skill").unwrap();
        assert_eq!(
            quarantined_learned_skill_names(dir.path()),
            vec!["flaky-skill".to_string()]
        );
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
        fs::write(
            hand.join("SKILL.md"),
            "---\nname: manual-skill\ndescription: x\n---\nbody\n",
        )
        .unwrap();
        // Learned skill with too few invocations.
        let fresh = dir.path().join(".claw/skills/learned/fresh-skill");
        fs::create_dir_all(&fresh).unwrap();
        fs::write(
            fresh.join("SKILL.md"),
            "---\nname: fresh-skill\ndescription: x\n---\nbody\n",
        )
        .unwrap();

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
