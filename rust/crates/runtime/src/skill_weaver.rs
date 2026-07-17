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
}
