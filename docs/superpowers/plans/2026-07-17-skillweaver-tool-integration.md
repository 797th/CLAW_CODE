# SkillWeaver ↔ Tool-Use Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close SkillWeaver's dead hone loop by auto-attributing skill outcomes at turn end, surface ledger evidence in the system-prompt skill index, and condense weave episodes to tool-call trajectories.

**Architecture:** All new logic lives in `rust/crates/runtime/src/skill_weaver.rs` (pending-window store, turn classifier, episode condensation) plus a ledger-aware `render_skill_index` in `prompt.rs`. The `tools` crate opens attribution windows (it already writes the ledger); the CLI turn driver closes them, since it is the only layer that knows the turn's outcome. Everything is advisory telemetry: no attribution failure may fail the Skill tool, the turn, or the prompt build.

**Tech Stack:** Rust workspace (existing), `serde_json` for the pending store (already a `runtime` dependency), no new crates.

**Spec:** `docs/superpowers/specs/2026-07-17-skillweaver-tool-integration-design.md`

## Global Constraints

- Verification from `rust/`: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Formatting: `scripts/fmt.sh` from repo root (never bare `cargo fmt`).
- `tools` depends on `runtime`, never the reverse. `commands` depends on `runtime`. Provider-free logic only in this plan — no `ApiClient` changes.
- All weaver-adjacent writes are atomic (tmp file + rename), matching `stats.json` discipline in `skill_weaver.rs`.
- Attribution is advisory: every save failure is swallowed (`let _ =`); missing/corrupt `pending.json` loads as empty.
- Attribution records *any* invoked skill name (including plugin-prefixed names like `superpowers:brainstorming`) — do NOT gate on `valid_skill_name`, which is the learned-skill kebab rule only. `hone()` already has the structural guard (it only renames files under `learned/`).
- Existing tests must keep passing — in particular `collect_episodes_skips_oversize_newest_file_and_keeps_older_ones` (I2 regression): raw-fallback episodes keep the skip-if-over-budget rule.
- Prompt index caps unchanged: `MAX_SKILL_INDEX_ENTRIES`, `MAX_SKILL_INDEX_BYTES`; annotations count against the byte budget (they're part of the entry string, so the existing accounting already covers them).

## File Structure

- Modify: `rust/crates/runtime/src/skill_weaver.rs` — pending-window store (`PendingInvocation`, open/clear/drain), turn classifier (`classify_turn_outcome`, `turn_tool_error_count`, `settle_attribution_windows`), episode condensation (`condense_session_content`).
- Modify: `rust/crates/runtime/src/prompt.rs` — `render_skill_index(skills, ledger)`, builder field `skill_ledger`, ledger load in `build_prompt`.
- Modify: `rust/crates/tools/src/lib.rs` — `record_skill_invocation` also opens the attribution window.
- Modify: `rust/crates/commands/src/lib.rs` — `run_skills_mark` clears pending windows for the marked skill.
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` — `run_turn_internal` settles windows on both Ok and terminal-Err paths.

Task order: 1 (store) → 2 (classifier/settle) → 3 (tools wiring) → 4 (CLI wiring) → 5 (mark clears pending) → 6 (ledger-aware index) → 7 (condensed episodes). Tasks 6 and 7 are independent of 3–5.

---

### Task 1: Pending attribution-window store

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs`

**Interfaces:**
- Produces: `pub const PENDING_FILENAME: &str = "pending.json"`, `pub const PENDING_MAX_AGE: Duration`, `pub struct PendingInvocation { pub skill: String, pub opened_ms: u64 }`, `pub fn open_attribution_window(weaver_dir: &Path, skill: &str)`, `pub fn clear_pending_for(weaver_dir: &Path, skill: &str)`, `pub fn drain_pending(weaver_dir: &Path) -> Vec<PendingInvocation>` (removes the file, discards stale entries).
- Consumes: existing `now_ms()`, `weaver_dir()` in the same module.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `skill_weaver.rs`:

```rust
    #[test]
    fn pending_windows_open_drain_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());

        // No file yet: drain is empty and does not error.
        assert!(drain_pending(&weaver).is_empty());

        open_attribution_window(&weaver, "fix-clippy-warnings");
        open_attribution_window(&weaver, "superpowers:brainstorming");
        open_attribution_window(&weaver, "   "); // blank: ignored

        let drained = drain_pending(&weaver);
        let names: Vec<&str> = drained.iter().map(|p| p.skill.as_str()).collect();
        assert_eq!(names, vec!["fix-clippy-warnings", "superpowers:brainstorming"]);
        assert!(drained.iter().all(|p| p.opened_ms > 0));
        // Drain removed the file.
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
        // One fresh, one ancient entry written directly.
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run (from `rust/`): `cargo test -p runtime skill_weaver::tests::pending -- --nocapture` and `cargo test -p runtime skill_weaver::tests::clear_pending`
Expected: compile FAILURE — `drain_pending`, `open_attribution_window`, `clear_pending_for`, `PENDING_FILENAME` not found.

- [ ] **Step 3: Implement the store**

In `skill_weaver.rs`, after the `SkillLedger` impl block, add a new section:

```rust
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
    fs::create_dir_all(weaver_dir)?;
    let dest = weaver_dir.join(PENDING_FILENAME);
    let tmp = weaver_dir.join(format!("{PENDING_FILENAME}.tmp"));
    let raw = serde_json::to_string_pretty(pending)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
    pending.retain(|p| p.skill != skill);
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
        .filter(|p| now.saturating_sub(p.opened_ms) <= max_age_ms)
        .collect()
}
```

`Duration`, `fs`, `io`, `Path` are already imported at the top of the module.

- [ ] **Step 4: Run tests to verify they pass**

Run (from `rust/`): `cargo test -p runtime skill_weaver`
Expected: all pass, including the four new tests.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs
git commit -m "feat(weaver): pending attribution-window store"
```

---

### Task 2: Turn classifier and settle

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs`

**Interfaces:**
- Consumes: Task 1's `drain_pending`; `crate::conversation::TurnSummary` (fields `tool_results: Vec<ConversationMessage>`); `crate::session::ContentBlock::ToolResult { is_error, .. }`.
- Produces:
  - `pub fn classify_turn_outcome(turn_failed: bool, tool_error_count: usize) -> Option<SkillOutcome>` — `turn_failed` → `Failure`; else 0 errors → `Success`, 1 error → `None` (ambiguous), ≥2 → `Failure` (cascade).
  - `pub fn turn_tool_error_count(summary: &crate::conversation::TurnSummary) -> usize`.
  - `pub fn settle_attribution_windows(cwd: &Path, outcome: Option<SkillOutcome>, opened_after_ms: u64) -> Vec<String>` — drains all windows, keeps only those opened at/after `opened_after_ms` (this turn), records `outcome` for each in the ledger, returns settled names. `outcome == None` still drains (records nothing).

- [ ] **Step 1: Write the failing tests**

Append to `skill_weaver.rs` tests:

```rust
    #[test]
    fn classify_turn_outcome_policy() {
        assert_eq!(classify_turn_outcome(true, 0), Some(SkillOutcome::Failure));
        assert_eq!(classify_turn_outcome(false, 0), Some(SkillOutcome::Success));
        assert_eq!(classify_turn_outcome(false, 1), None);
        assert_eq!(classify_turn_outcome(false, 2), Some(SkillOutcome::Failure));
    }

    #[test]
    fn settle_records_outcome_for_this_turns_windows_only() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        open_attribution_window(&weaver, "old-turn-skill");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let turn_start = now_ms();
        open_attribution_window(&weaver, "this-turn-skill");

        let settled = settle_attribution_windows(
            dir.path(),
            Some(SkillOutcome::Success),
            turn_start,
        );
        assert_eq!(settled, vec!["this-turn-skill".to_string()]);

        let ledger = SkillLedger::load(&weaver);
        assert_eq!(ledger.entry("this-turn-skill").unwrap().successes, 1);
        // Pre-turn window drained without judgment.
        assert!(ledger.entry("old-turn-skill").is_none());
        // Everything drained: settle again is a no-op.
        assert!(settle_attribution_windows(dir.path(), Some(SkillOutcome::Success), 0).is_empty());
    }

    #[test]
    fn settle_ambiguous_drains_without_recording() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = weaver_dir(dir.path());
        open_attribution_window(&weaver, "ambiguous-skill");

        let settled = settle_attribution_windows(dir.path(), None, 0);
        assert!(settled.is_empty());
        assert!(SkillLedger::load(&weaver).entry("ambiguous-skill").is_none());
        assert!(!weaver.join(PENDING_FILENAME).exists());
    }

    #[test]
    fn turn_tool_error_count_counts_error_results() {
        use crate::conversation::TurnSummary;
        use crate::session::{ContentBlock, ConversationMessage, MessageRole};
        let tool_message = |is_error: bool| ConversationMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "id".to_string(),
                tool_name: "bash".to_string(),
                output: "out".to_string(),
                is_error,
            }],
            usage: None,
        };
        let summary = TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: vec![tool_message(false), tool_message(true), tool_message(true)],
            prompt_cache_events: Vec::new(),
            iterations: 1,
            usage: Default::default(),
            auto_compaction: None,
            lifecycle_warnings: Vec::new(),
            gate_events: Vec::new(),
        };
        assert_eq!(turn_tool_error_count(&summary), 2);
    }
```

Note: if `TokenUsage` does not implement `Default`, replace `usage: Default::default()` with a zeroed literal (check `crates/runtime/src/usage.rs` for the field names, e.g. `TokenUsage { input_tokens: 0, output_tokens: 0, .. }` with whatever fields exist). Do not add a `Default` impl just for the test unless it's a one-line derive.

- [ ] **Step 2: Run tests to verify they fail**

Run (from `rust/`): `cargo test -p runtime skill_weaver`
Expected: compile FAILURE — `classify_turn_outcome`, `settle_attribution_windows`, `turn_tool_error_count` not found.

- [ ] **Step 3: Implement classifier and settle**

Add below the Task 1 section in `skill_weaver.rs`:

```rust
/// Heuristic outcome for every skill invoked this turn.
///
/// Conservative by design: one tool error is ambiguous (`None`, record
/// nothing) because a single failed probe (e.g. a grep with no matches
/// surfaced as an error) says little about the skill; a failed turn or an
/// error cascade is strong evidence against it. `hone()` needs
/// `QUARANTINE_MIN_INVOCATIONS` judged samples, so sparse-but-clean beats
/// dense-but-noisy.
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

/// Count error tool results in a completed turn's summary.
#[must_use]
pub fn turn_tool_error_count(summary: &crate::conversation::TurnSummary) -> usize {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter(|block| {
            matches!(
                block,
                crate::session::ContentBlock::ToolResult { is_error: true, .. }
            )
        })
        .count()
}

/// Close all attribution windows: drain, keep only windows opened during
/// this turn (`opened_ms >= opened_after_ms`), record `outcome` for each.
/// Windows from earlier turns are drained without judgment — attributing
/// turn N's outcome to turn N-1's skill would be noise. Returns the skill
/// names that were recorded. Advisory: all I/O failures degrade to a no-op.
pub fn settle_attribution_windows(
    cwd: &Path,
    outcome: Option<SkillOutcome>,
    opened_after_ms: u64,
) -> Vec<String> {
    let weaver = weaver_dir(cwd);
    let pending = drain_pending(&weaver);
    if pending.is_empty() {
        return Vec::new();
    }
    let Some(outcome) = outcome else {
        return Vec::new();
    };
    let mut ledger = SkillLedger::load(&weaver);
    let mut settled = Vec::new();
    for invocation in pending {
        if invocation.opened_ms < opened_after_ms {
            continue;
        }
        ledger.record(&invocation.skill, outcome);
        settled.push(invocation.skill);
    }
    if !settled.is_empty() {
        let _ = ledger.save(&weaver);
    }
    settled
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run (from `rust/`): `cargo test -p runtime skill_weaver`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs
git commit -m "feat(weaver): turn-outcome classifier and window settlement"
```

---

### Task 3: `execute_skill` opens the attribution window

**Files:**
- Modify: `rust/crates/tools/src/lib.rs` (function `record_skill_invocation`, near line 4158)

**Interfaces:**
- Consumes: Task 1's `runtime::skill_weaver::open_attribution_window(weaver_dir, skill)`.
- Produces: no new API — side effect only.

- [ ] **Step 1: Write the failing test**

In `crates/tools/src/lib.rs`, find the existing test module that exercises skills (search for `record_skill_invocation` or the tests around `execute_skill`). Add — adjusting the cwd-override mechanism to match how neighboring tests set the working directory (they use `std::env::set_current_dir` guards or a `CLAWD_`-style env var; follow the file's existing pattern exactly):

```rust
    #[test]
    fn record_skill_invocation_opens_attribution_window() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::set(dir.path()); // use this file's existing cwd-guard helper
        record_skill_invocation("$my-test-skill");

        let weaver = runtime::skill_weaver::weaver_dir(dir.path());
        let pending = runtime::skill_weaver::drain_pending(&weaver);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].skill, "my-test-skill"); // canonicalized: no $ prefix
        // Ledger still records Invoked, as before.
        let ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
        assert_eq!(ledger.entry("my-test-skill").unwrap().invocations, 1);
    }
```

If the file has no cwd-guard helper (cwd-dependent tests may be structured differently), test through the same seam the existing ledger behavior is tested through; if the existing `Invoked` recording has no test at all, add this one following whatever isolation pattern `$HOME`/cwd-sensitive tests in this crate use. (Memory note: this suite has known `$HOME` leakage issues — keep the test hermetic with explicit temp dirs.)

- [ ] **Step 2: Run test to verify it fails**

Run (from `rust/`): `cargo test -p tools record_skill_invocation`
Expected: FAIL — `drain_pending` returns empty (window never opened).

- [ ] **Step 3: Implement**

In `record_skill_invocation` (tools/src/lib.rs), add one line after the ledger save:

```rust
fn record_skill_invocation(skill: &str) {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    // Normalize the same way `resolve_skill_path` does (commands::resolve_skill_path)
    // so invocations like "$plan" or "/handoff" are recorded under the
    // canonical name and don't fragment ledger entries.
    let canonical = skill.trim().trim_start_matches('/').trim_start_matches('$');
    let weaver = runtime::skill_weaver::weaver_dir(&cwd);
    let mut ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
    ledger.record(canonical, runtime::skill_weaver::SkillOutcome::Invoked);
    let _ = ledger.save(&weaver);
    runtime::skill_weaver::open_attribution_window(&weaver, canonical);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run (from `rust/`): `cargo test -p tools record_skill_invocation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/tools/src/lib.rs
git commit -m "feat(tools): open attribution window on skill invocation"
```

---

### Task 4: CLI turn driver settles windows

**Files:**
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` (`run_turn_internal`, near line 9040)

**Interfaces:**
- Consumes: Task 2's `runtime::skill_weaver::{classify_turn_outcome, turn_tool_error_count, settle_attribution_windows, SkillOutcome}`.
- Produces: no new API — turn-end side effect. The TUI (`claw-tui/src/turn.rs`) is a follow-up outside this plan; the classifier lives in `runtime` so that wiring is one call when it lands.

- [ ] **Step 1: Add a helper and wire both outcome paths**

This task is wiring inside a 20k-line binary crate; the unit logic is already tested in Task 2, so no new unit test — verification is behavioral (Step 3). In the same `impl` block that contains `maybe_auto_weave_after_success`, add:

```rust
    /// Close skill attribution windows opened during this turn (SkillWeaver
    /// auto-attribution). Advisory telemetry — never fails the turn.
    fn settle_skill_attribution(&self, turn_started_ms: u64, outcome: Option<runtime::skill_weaver::SkillOutcome>) {
        let Ok(cwd) = env::current_dir() else {
            return;
        };
        let _ = runtime::skill_weaver::settle_attribution_windows(&cwd, outcome, turn_started_ms);
    }
```

In `run_turn_internal`, capture the turn start time right before `runtime.run_turn(...)` is called (around line 9063):

```rust
        let turn_started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let result = runtime.run_turn(input, Some(&mut permission_prompter));
```

In the `Ok(summary)` arm, immediately before `self.maybe_auto_dream_after_success();`:

```rust
                self.settle_skill_attribution(
                    turn_started_ms,
                    runtime::skill_weaver::classify_turn_outcome(
                        false,
                        runtime::skill_weaver::turn_tool_error_count(&summary),
                    ),
                );
```

In the `Err(error)` arm, after the `is_context_window` / `is_no_content` flags are computed (around line 9138), settle Failure ONLY when we are not entering the auto-compact retry path — a context-overflow retry may still finish the turn successfully, and its own completion path settles then (windows survive because `pending.json` persists; `PENDING_MAX_AGE` bounds the worst case):

```rust
                if !(is_context_window || is_no_content) {
                    self.settle_skill_attribution(
                        turn_started_ms,
                        Some(runtime::skill_weaver::SkillOutcome::Failure),
                    );
                }
```

Place it right after the flag computation, before the `if is_context_window || is_no_content {` block.

- [ ] **Step 2: Compile clean**

Run (from `rust/`): `cargo clippy -p rusty-claude-cli --all-targets -- -D warnings`
Expected: clean. (If `settle_skill_attribution` takes `&self` but the arm only has `&mut self` available, `&self` is fine — `&mut self` derefs. If borrow-checker friction arises from `summary`, compute the classification into a local before the helper call.)

- [ ] **Step 3: Behavioral verification**

Run (from `rust/`): `cargo test -p rusty-claude-cli` — existing turn tests must stay green.
Then a smoke check that the seam fires: from a scratch temp dir with a fake skill, run the debug binary for one turn against the mock provider if the repo's `mock-anthropic-service` harness makes that easy; otherwise verify via the unit seams (Task 2 covers the logic; this step only confirms compilation and no test regressions).

- [ ] **Step 4: Commit**

```bash
git add rust/crates/rusty-claude-cli/src/main.rs
git commit -m "feat(cli): settle skill attribution windows at turn end"
```

---

### Task 5: `/skills mark` clears pending windows

**Files:**
- Modify: `rust/crates/commands/src/lib.rs` (`run_skills_mark`, near line 5457)

**Interfaces:**
- Consumes: Task 1's `runtime::skill_weaver::clear_pending_for`.
- Produces: no new API.

- [ ] **Step 1: Write the failing test**

In `commands/src/lib.rs`, next to the existing `run_skills_mark` tests (search `skills_mark`), add:

```rust
    #[test]
    fn skills_mark_clears_pending_window_for_that_skill() {
        let dir = tempfile::tempdir().unwrap();
        let weaver = runtime::skill_weaver::weaver_dir(dir.path());
        runtime::skill_weaver::open_attribution_window(&weaver, "marked-skill");
        runtime::skill_weaver::open_attribution_window(&weaver, "other-skill");

        run_skills_mark(dir.path(), "marked-skill", "success").unwrap();

        let pending = runtime::skill_weaver::drain_pending(&weaver);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].skill, "other-skill");
        // Explicit mark recorded.
        let ledger = runtime::skill_weaver::SkillLedger::load(&weaver);
        assert_eq!(ledger.entry("marked-skill").unwrap().successes, 1);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `rust/`): `cargo test -p commands skills_mark_clears_pending`
Expected: FAIL — pending still has 2 entries.

- [ ] **Step 3: Implement**

In `run_skills_mark`, after `ledger.save(...)` succeeds, add:

```rust
    // The explicit verdict overrides heuristic attribution: drop any open
    // window so turn-end settlement doesn't double-count this invocation.
    runtime::skill_weaver::clear_pending_for(&weaver, skill);
```

- [ ] **Step 4: Run test to verify it passes**

Run (from `rust/`): `cargo test -p commands skills_mark`
Expected: PASS (including pre-existing mark tests).

- [ ] **Step 5: Commit**

```bash
git add rust/crates/commands/src/lib.rs
git commit -m "feat(commands): explicit skill mark clears pending attribution"
```

---

### Task 6: Ledger-aware skill index

**Files:**
- Modify: `rust/crates/runtime/src/prompt.rs` (`render_skill_index` near line 761, `SystemPromptBuilder` fields/`build()` near lines 222/311, `build_prompt` near line 740, existing tests near line 1697)

**Interfaces:**
- Consumes: `crate::skill_weaver::{SkillLedger, weaver_dir, QUARANTINE_MIN_INVOCATIONS}`; `SkillMeta { name, description, path }`.
- Produces:
  - `pub fn render_skill_index(skills: &[SkillMeta], ledger: Option<&SkillLedger>) -> Option<String>` (signature change — update all callers).
  - `SystemPromptBuilder::with_skill_ledger(mut self, ledger: SkillLedger) -> Self`.
  - Entry format: hand-written unchanged (`- name: description`); learned skills `- name (learned, S/J ok): description` where `S = successes`, `J = successes + failures`, when the record has `invocations >= QUARANTINE_MIN_INVOCATIONS` and `J > 0`; otherwise `- name (learned, unproven): description`. "Learned" = any path component equal to `learned`.

- [ ] **Step 1: Write the failing tests**

In the `prompt.rs` test module, extend the `skill()` helper family and add tests. The existing helper builds paths like `/tmp/.claw/skills/{name}.md`; add a learned variant:

```rust
    fn learned_skill(name: &str, description: &str) -> SkillMeta {
        SkillMeta {
            name: name.to_string(),
            description: description.to_string(),
            path: PathBuf::from(format!("/tmp/.claw/skills/learned/{name}/SKILL.md")),
        }
    }

    #[test]
    fn skill_index_annotates_learned_skills_with_ledger_evidence() {
        use crate::skill_weaver::{SkillLedger, SkillOutcome};
        let dir = tempfile::tempdir().unwrap();
        let weaver = crate::skill_weaver::weaver_dir(dir.path());
        let mut ledger = SkillLedger::load(&weaver);
        for _ in 0..3 {
            ledger.record("proven-skill", SkillOutcome::Invoked);
        }
        ledger.record("proven-skill", SkillOutcome::Success);
        ledger.record("proven-skill", SkillOutcome::Success);
        ledger.record("proven-skill", SkillOutcome::Failure);

        let skills = vec![
            skill("hand-written", "A manual skill"),
            learned_skill("proven-skill", "Earned its keep"),
            learned_skill("new-skill", "Just woven"),
        ];
        let rendered = render_skill_index(&skills, Some(&ledger)).unwrap();

        assert!(rendered.contains("- hand-written: A manual skill"));
        assert!(rendered.contains("- proven-skill (learned, 2/3 ok): Earned its keep"));
        assert!(rendered.contains("- new-skill (learned, unproven): Just woven"));
    }

    #[test]
    fn skill_index_without_ledger_marks_learned_as_unproven() {
        let skills = vec![learned_skill("mystery-skill", "No ledger data")];
        let rendered = render_skill_index(&skills, None).unwrap();
        assert!(rendered.contains("- mystery-skill (learned, unproven): No ledger data"));
    }
```

Also update every existing `render_skill_index(&skills)` call in tests to `render_skill_index(&skills, None)` (there are several: the invocation-instruction test, the empty-slice test, truncation tests).

- [ ] **Step 2: Run tests to verify they fail**

Run (from `rust/`): `cargo test -p runtime prompt::tests::skill_index`
Expected: compile FAILURE — wrong arity for `render_skill_index`.

- [ ] **Step 3: Implement**

In `prompt.rs`:

1. Import: add `use crate::skill_weaver::{SkillLedger, QUARANTINE_MIN_INVOCATIONS};` near the existing `use crate::harness_assets::SkillMeta;`.

2. Add builder field and setter (next to `skills: Vec<SkillMeta>` at line ~222 and `with_skills` at ~271):

```rust
    skill_ledger: Option<SkillLedger>,
```
```rust
    #[must_use]
    pub fn with_skill_ledger(mut self, ledger: SkillLedger) -> Self {
        self.skill_ledger = Some(ledger);
        self
    }
```
Initialize `skill_ledger: None` wherever the builder's other fields are defaulted (its `new()`/`Default`).

3. In `build()` (line ~311):

```rust
        if let Some(skill_index) = render_skill_index(&self.skills, self.skill_ledger.as_ref()) {
            sections.push(skill_index);
        }
```

4. In `build_prompt` (line ~740), load the ledger next to skill discovery and thread it in:

```rust
    let skills = crate::harness_assets::discover(&cwd).skills;
    let skill_ledger = SkillLedger::load(&crate::skill_weaver::weaver_dir(&cwd));
    let sections = SystemPromptBuilder::new()
        .with_os(os_name, os_version)
        .with_model_family(model_family)
        .with_project_context(project_context.clone())
        .with_memory_prompt(memory_prompt)
        .with_runtime_config(config)
        .with_skills(skills)
        .with_skill_ledger(skill_ledger)
        .build();
```

5. Change `render_skill_index`:

```rust
#[must_use]
pub fn render_skill_index(skills: &[SkillMeta], ledger: Option<&SkillLedger>) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines = vec![
        "# Available skills".to_string(),
        "Invoke with the Skill tool before acting when a task matches:".to_string(),
    ];

    let header_len: usize = lines.iter().map(|line| line.len() + 1).sum();
    let mut budget = MAX_SKILL_INDEX_BYTES.saturating_sub(header_len);
    let note_len = SKILL_INDEX_TRUNCATION_NOTE.len() + 1;
    let mut truncated = skills.len() > MAX_SKILL_INDEX_ENTRIES;

    for skill in skills.iter().take(MAX_SKILL_INDEX_ENTRIES) {
        let entry = match skill_index_annotation(skill, ledger) {
            Some(annotation) => format!("- {} {}: {}", skill.name, annotation, skill.description),
            None => format!("- {}: {}", skill.name, skill.description),
        };
        let entry_len = entry.len() + 1;
        // Reserve room for the truncation note in case we need to stop early.
        if entry_len + note_len > budget {
            truncated = true;
            break;
        }
        budget -= entry_len;
        lines.push(entry);
    }

    if truncated {
        lines.push(SKILL_INDEX_TRUNCATION_NOTE.to_string());
    }

    Some(lines.join("\n"))
}

/// Evidence annotation for learned skills; hand-written skills get none.
/// "Learned" is structural: any `learned` path component (matches the
/// weaver's write location `.claw/skills/learned/<name>/SKILL.md`).
fn skill_index_annotation(skill: &SkillMeta, ledger: Option<&SkillLedger>) -> Option<String> {
    let learned = skill
        .path
        .components()
        .any(|component| component.as_os_str() == "learned");
    if !learned {
        return None;
    }
    if let Some(record) = ledger.and_then(|l| l.entry(&skill.name)) {
        let judged = record.successes + record.failures;
        if record.invocations >= QUARANTINE_MIN_INVOCATIONS && judged > 0 {
            return Some(format!("(learned, {}/{} ok)", record.successes, judged));
        }
    }
    Some("(learned, unproven)".to_string())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run (from `rust/`): `cargo test -p runtime prompt`
Expected: PASS, including all pre-existing skill-index tests (now called with `None`).

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/prompt.rs
git commit -m "feat(prompt): annotate learned skills with ledger evidence"
```

---

### Task 7: Condensed episodes for the weave pass

**Files:**
- Modify: `rust/crates/runtime/src/skill_weaver.rs` (`collect_episodes` near line 160)
- Test additions: same module + `rust/crates/runtime/tests/skill_weaver_pass.rs` stays green unchanged

**Interfaces:**
- Consumes: `crate::session::{Session, ContentBlock, ConversationMessage, MessageRole}`; `Session::load_from_path(path)`; `session.messages` (pub field).
- Produces: `fn condense_session_content(path: &Path) -> Option<String>` (private); `collect_episodes` behavior change — parseable session files yield condensed trajectories, unparseable files fall back to raw content with the existing skip-if-over-budget rule.

- [ ] **Step 1: Write the failing tests**

Append to `skill_weaver.rs` tests:

```rust
    #[test]
    fn collect_episodes_condenses_parseable_sessions_to_tool_trajectories() {
        use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};
        let dir = tempfile::tempdir().unwrap();
        let sessions = crate::session::workspace_sessions_dir(dir.path()).unwrap();
        fs::create_dir_all(&sessions).unwrap();

        let mut session = Session::new();
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
        // Trajectory structure survives...
        assert!(content.contains("user: Fix the clippy warnings"));
        assert!(content.contains("tool bash:"));
        assert!(content.contains("result bash ok:"));
        // ...but payloads are snipped, so condensed is much smaller than raw.
        assert!(content.len() < raw_len / 2, "condensed {} vs raw {}", content.len(), raw_len);
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
```

The pre-existing tests (`collect_episodes_reads_newest_first_and_respects_budget`, `collect_episodes_skips_oversize_newest_file_and_keeps_older_ones`, `collect_episodes_since_filters_old_sessions`) use plain-text files, which are unparseable as sessions — they exercise the raw fallback and must stay green untouched.

- [ ] **Step 2: Run tests to verify they fail**

Run (from `rust/`): `cargo test -p runtime skill_weaver`
Expected: `collect_episodes_condenses...` FAILS (content is raw JSONL: no `tool bash:` line, size not halved). The fallback test passes already (current behavior) — that's fine; it pins the contract.

- [ ] **Step 3: Implement condensation**

In `skill_weaver.rs`, above `collect_episodes`:

```rust
/// Per-line payload cap in condensed trajectories. Big enough to keep exact
/// commands, small enough that tool payloads can't eat the episode budget.
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

/// Condense a session file into a tool-call trajectory: user goals, tool
/// calls (name + snipped args), results (ok/error + snipped output), and
/// assistant text. Returns `None` when the file doesn't parse as a session
/// or holds no messages — callers fall back to raw content.
fn condense_session_content(path: &Path) -> Option<String> {
    use crate::session::{ContentBlock, MessageRole, Session};
    let session = Session::load_from_path(path).ok()?;
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
                (_, ContentBlock::ToolResult { tool_name, output, is_error, .. }) => {
                    let status = if *is_error { "error" } else { "ok" };
                    push_condensed_line(&mut out, &format!("result {tool_name} {status}"), output);
                }
                // Thinking blocks and system messages carry no recipe signal.
                _ => {}
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}
```

Then in `collect_episodes`, replace the read inside the budget loop:

```rust
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
```

Note: `Session::load_from_path` attaches a persistence path but `condense_session_content` never mutates the session, so nothing is written back.

- [ ] **Step 4: Run tests to verify they pass**

Run (from `rust/`): `cargo test -p runtime skill_weaver` and `cargo test -p runtime --test skill_weaver_pass`
Expected: PASS, including all pre-existing episode/budget/weave-pass tests.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/runtime/src/skill_weaver.rs
git commit -m "feat(weaver): condense episodes to tool-call trajectories"
```

---

### Task 8: Full-workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run (from repo root): `scripts/fmt.sh`
Expected: clean or auto-fixed; re-stage anything it touched.

- [ ] **Step 2: Clippy**

Run (from `rust/`): `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 3: Full test suite**

Run (from `rust/`): `cargo test --workspace`
Expected: all green. (Known suite caveat: some pre-existing tests are sensitive to `$HOME` leakage — a failure that reproduces on a clean checkout without this branch is not a regression from this work; verify by stashing.)

- [ ] **Step 4: Commit any format-only fallout**

```bash
git add -A rust/
git commit -m "chore: fmt/clippy fallout for skillweaver integration" || true
```
