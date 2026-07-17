# SkillWeaver ↔ Tool-Use Integration Design

**Date:** 2026-07-17
**Status:** Approved by user (design review)

## Problem

SkillWeaver (`rust/crates/runtime/src/skill_weaver.rs`) is fully built — ledger,
weave pass, quarantine, gates — but wired into tool use at exactly one line:
`record_skill_invocation()` in `rust/crates/tools/src/lib.rs`, which records
`SkillOutcome::Invoked` and nothing else.

Consequence: `hone()` skips any skill where `successes + failures == 0`. The
only writer of `Success`/`Failure` is the manual `/skills mark` slash command.
The propose → synthesize → **hone** → transfer loop never hones; quarantine is
unreachable without a human hand-marking outcomes.

Contrast: caveman mode is threaded through every layer where its concern flows
(`prompt.rs` system-prompt section, `conversation.rs` message building,
`claw-engine/client.rs` wire conversion, `claw-tui/turn.rs`, CLI output sink).
SkillWeaver should be integrated the same way: at every point where skill
outcomes and skill evidence flow, not one fire-and-forget line.

## Design

Three pieces, ordered by dependency.

### Piece 1 — Auto-attribution: close the hone loop

Track a skill's outcome across the turn it was invoked in.

- New attribution tracker in `runtime` (module `skill_attribution` or a
  section of `skill_weaver.rs`). `execute_skill` opens an "attribution
  window": a pending record (skill name, timestamp) persisted at
  `.claw/skills/.weaver/pending.json` (atomic write, same discipline as
  `stats.json`). Persisted rather than in-memory because the `tools` crate
  has no handle on turn state, and `tools` depends on `runtime`, never the
  reverse.
- The window is closed at turn end by the turn loop (near
  `conversation.rs` / the CLI turn driver, alongside where
  `maybe_run_auto_weave` is already wired at session end):
  - **Failure** — after the skill invocation the turn hit a tool-error
    cascade, the user interrupted, or the turn ended in an API/tool error.
  - **Success** — the turn completed cleanly (assistant final message, no
    unresolved tool errors) after the invocation.
  - **Ambiguous** — record nothing. Conservative by design: `hone()` needs
    ≥ `QUARANTINE_MIN_INVOCATIONS` judged samples; noise is worse than
    sparseness.
- Stale pending records (e.g. process crash mid-turn) are discarded on next
  load, never counted as failures.
- `/skills mark` stays as the manual override; an explicit mark for a skill
  clears any pending window for it.

### Piece 2 — Ledger feeds the prompt

Mirror caveman's always-on prompt injection: `render_skill_index`
(`prompt.rs`) reads the ledger.

- `build_prompt` already calls `harness_assets::discover(cwd)`; also load
  `SkillLedger` there (one small JSON read).
- Learned skills (under `.claw/skills/learned/`) render with evidence:
  `- fix-clippy-warnings (learned, 5/6 ok): description`.
- Learned skills with fewer than `QUARANTINE_MIN_INVOCATIONS` invocations
  render `(learned, unproven)`.
- Hand-written skills render unchanged (bare `- name: description`).
- Quarantined skills already drop out of discovery — no change.
- Existing size caps (`MAX_SKILL_INDEX_ENTRIES`, `MAX_SKILL_INDEX_BYTES`)
  still apply; the annotation counts against the byte budget.

### Piece 3 — Weaver reads tool-call structure

`synthesize_skills` currently dumps raw session JSONL at the provider —
wasteful (payloads eat `max_input_bytes`) and noisy.

- `collect_episodes` parses each session JSONL into a condensed trajectory:
  user goal, ordered tool calls (tool name + key args + ok/error), final
  outcome. Session structs live in this crate (`session.rs`); no new
  parsing surface.
- The condensed form is what `synthesize_skills` sends. More sessions fit
  the byte budget and woven skills come out as concrete tool recipes.
- Unparseable lines degrade to being skipped (never fail the episode); a
  fully unparseable file falls back to raw content, truncated to budget.

### Explicitly out of scope

- Returning track-record data from the `Skill` tool result — redundant once
  Piece 2 puts the same evidence in the prompt index (YAGNI).
- Any change to weave gates, quarantine thresholds, or the provider prompt
  contract (`<skill name="...">` blocks).

## Files touched

- `rust/crates/runtime/src/skill_weaver.rs` — attribution window types +
  pending-file I/O; episode condensation.
- `rust/crates/runtime/src/prompt.rs` — ledger-aware `render_skill_index`.
- `rust/crates/rusty-claude-cli/src/main.rs` — close attribution windows at
  turn end in the CLI turn driver (the layer that already knows the turn's
  outcome and already wires `maybe_run_auto_weave`). The classification
  helper (turn outcome → `SkillOutcome`) lives in `runtime` so the TUI can
  reuse it; a follow-up wires `claw-tui/src/turn.rs` the same way.
- `rust/crates/tools/src/lib.rs` — `record_skill_invocation` also opens the
  attribution window.
- `rust/crates/commands/src/lib.rs` — `/skills mark` clears pending windows.

## Error handling

Same posture as the existing ledger: attribution is advisory telemetry.
Missing/corrupt `pending.json` loads as empty; save failures are swallowed
(`let _ =`); nothing in the attribution path may fail the Skill tool, the
turn, or the prompt build.

## Testing

- `#[cfg(test)]` next to code, plus `runtime/tests/skill_weaver_pass.rs`
  extensions.
- Key cases: clean turn after invocation → Success; tool-error turn →
  Failure; ambiguous turn → nothing recorded; stale pending discarded;
  manual mark clears pending; ledger stats render in skill index with byte
  budget respected; hand-written skills unannotated; condensed episodes
  smaller than raw and under budget; unparseable session falls back to raw.
- Verification (from `rust/`): `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`; formatting via `scripts/fmt.sh` from repo root.
