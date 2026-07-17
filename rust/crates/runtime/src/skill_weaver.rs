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
