use std::fs;

use runtime::{ApiClient, ApiRequest, AssistantEvent, RuntimeError};

// Reuse the mock ApiClient pattern from the dreamer tests (crates/runtime/src/dreamer.rs
// `StubClient`); the mock returns `MOCK_RESPONSE` as a single text event.
struct StubClient {
    response: String,
}

impl ApiClient for StubClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        Ok(vec![
            AssistantEvent::TextDelta(self.response.clone()),
            AssistantEvent::MessageStop,
        ])
    }
}

fn mock_client_returning(response: &str) -> StubClient {
    StubClient {
        response: response.to_string(),
    }
}

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
    let sessions = runtime::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("s1.jsonl"),
        "user asked to fix clippy; agent ran clippy, fixed, tests passed\n",
    )
    .unwrap();

    let mut client = mock_client_returning(MOCK_RESPONSE);
    let run = runtime::skill_weaver::run_weave_pass(dir.path(), &mut client, 64 * 1024).unwrap();

    assert_eq!(run.episode_count, 1);
    let skill_path = dir
        .path()
        .join(".claw/skills/learned/fix-clippy-warnings/SKILL.md");
    assert!(skill_path.is_file());
    let contents = fs::read_to_string(&skill_path).unwrap();
    assert!(contents.starts_with("---"));
    // Discovery must see it.
    let assets = runtime::harness_assets::discover(dir.path());
    assert!(assets.skills.iter().any(|s| s.name == "fix-clippy-warnings"));
    // Marker written.
    assert!(runtime::skill_weaver::weaver_dir(dir.path())
        .join(".last-weave")
        .is_file());
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
    let sessions = runtime::workspace_sessions_dir(dir.path()).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("s1.jsonl"), "trajectory\n").unwrap();

    let mut client = mock_client_returning(bad);
    let err = runtime::skill_weaver::run_weave_pass(dir.path(), &mut client, 64 * 1024);
    assert!(err.is_err() || !dir.path().join(".claw/skills/learned").exists());
}
