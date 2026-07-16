# SAW-Style Agentic Harness Integration Plan for Claw Code

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate a three-layer agentic harness (Hooks → Commands → Skills, plus role-gated workflow) modeled on [bybren-llc/safe-agentic-workflow](https://github.com/bybren-llc/safe-agentic-workflow) (SAW) into the `clawcli` Rust CLI, so the agent enforces quality gates, discovers project skills automatically, and supports user-defined workflow commands.

**Architecture:** SAW's model is three layers: (1) **Hooks** — automatic guardrails fired on lifecycle events, (2) **Commands** — user-invoked workflow slash commands, (3) **Skills** — model-invoked expertise packs, plus a **role/gate workflow** (spec gate before implementation, QA gate before PR, stop-the-line authority). Claw Code already has partial primitives: 3 hook events (`PreToolUse`/`PostToolUse`/`PostToolUseFailure` in `crates/runtime/src/hooks.rs`), a `Skill` tool with frontmatter parsing and lookup roots (`crates/tools/src/lib.rs:4140-4231`), a hardcoded slash-command enum (`crates/commands/src/lib.rs`), a Phase-1 multi-agent orchestrator (`crates/rusty-claude-cli/src/mao.rs`), a `PolicyRule` engine (`crates/runtime/src/policy_engine.rs`), and `TaskPacket`/`task_registry`. This plan extends those primitives instead of adding a parallel system: expand hook lifecycle events + JSON hook protocol, add a `.claw/` harness asset convention (agents/commands/skills/hooks), load markdown slash commands dynamically, surface a skill index into the system prompt, and add a gate-enforcing workflow state machine wired into `policy_engine` and `mao`.

**Tech Stack:** Rust (existing workspace at `rust/`), serde/serde_json, existing crates: `runtime`, `commands`, `tools`, `rusty-claude-cli`. No new external dependencies.

## Global Constraints

- All work happens in the `rust/` workspace; verify with `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` from `rust/` (per repo `CLAUDE.md`).
- Formatting via `scripts/fmt.sh --check` from repo root.
- Harness asset directory name is `.claw/` (project-level) and `$CLAW_CONFIG_HOME` (user-level) — reuse the existing skill lookup roots in `crates/tools/src/lib.rs::skill_lookup_roots()`; do not invent a new root scheme.
- Hook protocol is JSON-over-stdin/stdout, mirroring Claude Code's contract (exit 0 = allow, exit 2 = block with stderr as reason, JSON stdout for context injection) — keeps compatibility with existing `.claude/`-style hook scripts.
- No breaking changes to existing `.claw.json` / `RuntimeHookConfig` TOML/JSON shapes: new fields are optional with `#[serde(default)]`.
- Gates must be **advisory by default, blocking when configured** — a new user without a `.claw/workflow.json` sees zero behavior change.
- Small, reviewable commits; each task independently testable.

---

## Gap Analysis (what SAW has vs. what Claw Code has)

| SAW capability | Claw Code today | Gap |
|---|---|---|
| Lifecycle hooks: SessionStart, SessionEnd, UserPromptSubmit, Stop, pre-commit/pre-push guidance | Only PreToolUse / PostToolUse / PostToolUseFailure (`hooks.rs`) | Add 5 lifecycle events + matcher support |
| Hook decisions (block, inject context) | `HookPermissionDecision = PermissionOverride` on PreToolUse only | Add JSON stdout protocol: `decision`, `reason`, `additionalContext` for all events |
| 24 user-defined workflow slash commands (markdown files) | Hardcoded `Command` enum only (`commands/src/lib.rs:1240+`) | Dynamic markdown commands from `.claw/commands/*.md` |
| 18 model-invoked skills, auto-triggered | `Skill` tool loads by name; **model never told which skills exist** | Inject skill index (name + description frontmatter) into system prompt |
| 11 role agent profiles with gate authority | `mao.rs` Phase 1: 4 hardcoded personas, no gates | Load personas from `.claw/agents/*.md`; add reviewer/QAS role type |
| Stop-the-Line gate: no implementation without acceptance criteria; QAS blocking gate before PR | `policy_engine.rs` has `PolicyRule`/`PolicyCondition`/`PolicyAction` but nothing drives a workflow lifecycle | Workflow state machine (`spec → implement → verify → review → done`) with gate rules evaluated via policy engine |
| Evidence-based delivery (artifacts per gate) | `lane_events.rs`, NDJSON output exist | Emit `gate_check` events with evidence refs into existing NDJSON stream |
| Bootstrap of harness assets | `clawcli init` writes CLAUDE.md only | `clawcli init --harness` scaffolds `.claw/` with a SAW-lite starter pack |

---

## File Structure

```
rust/crates/runtime/src/
  hooks.rs                    # MODIFY: new HookEvent variants + JSON decision protocol
  config.rs                   # MODIFY: RuntimeHookConfig gains new optional event lists
  workflow.rs                 # CREATE: WorkflowPhase state machine + GateCheck
  harness_assets.rs           # CREATE: discovery/validation of .claw/{agents,commands,skills,hooks}
  policy_engine.rs            # MODIFY: add WorkflowGate condition/action variants
  prompt.rs                   # MODIFY: inject skill index + active workflow phase into system prompt
  lib.rs                      # MODIFY: export new modules

rust/crates/commands/src/
  lib.rs                      # MODIFY: Command::Custom variant + markdown command loader
                              #         (+ /workflow status|advance|gate command)

rust/crates/rusty-claude-cli/src/
  main.rs                     # MODIFY: wire lifecycle hooks (session start/end, prompt submit, stop)
  mao.rs                      # MODIFY: persona loading from .claw/agents, reviewer gate pass
  init.rs                     # MODIFY: --harness scaffolding

rust/crates/runtime/tests/
  workflow_gates.rs           # CREATE: gate state machine tests
  harness_assets.rs           # CREATE: asset discovery tests
rust/crates/commands/tests/
  custom_commands.rs          # CREATE: markdown command loading tests
```

---

## Phase 1 — Hook Layer Expansion (guardrails)

### Task 1: New lifecycle hook events

**Files:**
- Modify: `rust/crates/runtime/src/hooks.rs` (HookEvent enum, ~line 22)
- Modify: `rust/crates/runtime/src/config.rs` (`RuntimeHookConfig`, lines 240–246 and its `impl` at 1273+)
- Test: `rust/crates/runtime/src/hooks.rs` (inline `#[cfg(test)]`, matching existing test style)

**Interfaces:**
- Consumes: existing `RuntimeHookCommand`, `HookEvent::as_str()`.
- Produces: `HookEvent::{SessionStart, SessionEnd, UserPromptSubmit, Stop, PreCompact}` and `RuntimeHookConfig::{session_start(), session_end(), user_prompt_submit(), stop(), pre_compact()}` accessors returning `&[RuntimeHookCommand]`. Later tasks (Task 3, Task 10) call these.

- [ ] **Step 1: Write failing tests** — new variants serialize to Claude-Code-compatible strings, and config parses new optional lists:

```rust
#[test]
fn lifecycle_hook_events_have_canonical_names() {
    assert_eq!(HookEvent::SessionStart.as_str(), "SessionStart");
    assert_eq!(HookEvent::SessionEnd.as_str(), "SessionEnd");
    assert_eq!(HookEvent::UserPromptSubmit.as_str(), "UserPromptSubmit");
    assert_eq!(HookEvent::Stop.as_str(), "Stop");
    assert_eq!(HookEvent::PreCompact.as_str(), "PreCompact");
}

#[test]
fn hook_config_parses_lifecycle_lists_and_defaults_empty() {
    let cfg: RuntimeHookConfig = serde_json::from_value(serde_json::json!({
        "session_start": ["echo hi"],
    })).unwrap();
    assert_eq!(cfg.session_start().len(), 1);
    assert!(cfg.stop().is_empty());
}
```

- [ ] **Step 2: Run** `cargo test -p runtime lifecycle_hook` → FAIL (variants missing).
- [ ] **Step 3: Implement** — add the five variants to `HookEvent` + `as_str()`, add `#[serde(default)] session_start: Vec<RuntimeHookCommand>` (and the other four) to `RuntimeHookConfig` with accessors, mirroring the existing `pre_tool_use` pattern exactly.
- [ ] **Step 4: Run** `cargo test -p runtime` → PASS; `cargo clippy -p runtime --all-targets -- -D warnings` → clean.
- [ ] **Step 5: Commit** `feat(runtime): add lifecycle hook events (SessionStart/SessionEnd/UserPromptSubmit/Stop/PreCompact)`

### Task 2: JSON hook decision protocol

**Files:**
- Modify: `rust/crates/runtime/src/hooks.rs` (hook execution path — the fn that spawns `Command` with `Stdio`)
- Test: inline `#[cfg(test)]` in `hooks.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookOutcome {
    pub decision: HookDecision,        // Allow | Block { reason: String }
    pub additional_context: Option<String>, // injected into next model turn
}

pub enum HookDecision { Allow, Block { reason: String } }
```

- Contract: hook process receives JSON on stdin (`{"hook_event_name": "...", "tool_name": "...", "tool_input": {...}, "prompt": "...", "cwd": "..."}` — fields present when applicable). Exit 0 → Allow; exit 2 → Block with stderr as reason; if stdout is valid JSON with `{"decision":"block","reason":...}` or `{"additionalContext":"..."}`, that wins over exit-code inference. Non-JSON stdout on lifecycle events becomes `additional_context` (matches SAW's SessionStart "show guidance" hooks).

- [ ] **Step 1: Write failing tests** using shell one-liners as hook commands:

```rust
#[test]
fn exit_two_blocks_with_stderr_reason() {
    let out = run_hook_command_for_test("sh -c 'echo nope >&2; exit 2'", HookEvent::PreToolUse, &json!({"tool_name":"Bash"}));
    assert!(matches!(out.decision, HookDecision::Block { ref reason } if reason.contains("nope")));
}

#[test]
fn stdout_text_on_session_start_becomes_context() {
    let out = run_hook_command_for_test("echo 'workflow: run /start-work first'", HookEvent::SessionStart, &json!({}));
    assert_eq!(out.decision, HookDecision::Allow);
    assert!(out.additional_context.unwrap().contains("/start-work"));
}

#[test]
fn json_stdout_decision_overrides_exit_code() {
    let out = run_hook_command_for_test(r#"echo '{"decision":"block","reason":"missing AC"}'"#, HookEvent::Stop, &json!({}));
    assert!(matches!(out.decision, HookDecision::Block { .. }));
}
```

(Gate these tests `#[cfg(unix)]` and mirror how existing hook tests spawn commands.)
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — refactor the existing hook runner to write the JSON payload to child stdin, capture stdout/stderr, and map to `HookOutcome` per the contract above. Keep the existing `HookPermissionDecision` path working by mapping `Block` → deny override for PreToolUse.
- [ ] **Step 4: Run** `cargo test -p runtime hooks` → PASS.
- [ ] **Step 5: Commit** `feat(runtime): JSON hook decision protocol (block/allow/context injection)`

### Task 3: Fire lifecycle hooks from the CLI loop

**Files:**
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` (session boot, prompt submit, turn end, exit paths)
- Test: `rust/crates/rusty-claude-cli/tests/lifecycle_hooks.rs` (integration, using a temp `.claw.json` with `sh -c` hooks writing marker files)

**Interfaces:**
- Consumes: `RuntimeHookConfig` accessors from Task 1, `HookOutcome` from Task 2.
- Semantics: `SessionStart` context is appended to the first system/user turn; `UserPromptSubmit` Block cancels the turn and prints the reason; `Stop` Block re-prompts the model with the reason (this is the mechanism the QAS gate in Task 9 uses — "you may not stop; verification incomplete"); `SessionEnd` is fire-and-forget on exit. Cap `Stop`-block loops at 3 consecutive blocks to prevent infinite loops, then warn and allow stop.

- [ ] **Step 1: Write failing integration test** — run the CLI in non-interactive one-shot mode (existing `--output-format json` path) with a config whose `session_start` hook writes `hook_ran` to a temp file; assert file exists after run and that hook stdout appears in the transcript context.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** wiring in `main.rs` at the four call sites; reuse the existing `HookProgressReporter` so the TUI shows hook activity.
- [ ] **Step 4: Run** `cargo test -p rusty-claude-cli lifecycle` → PASS.
- [ ] **Step 5: Commit** `feat(cli): fire SessionStart/UserPromptSubmit/Stop/SessionEnd hooks in agent loop`

---

## Phase 2 — Harness Asset Convention (`.claw/` directory)

### Task 4: Harness asset discovery module

**Files:**
- Create: `rust/crates/runtime/src/harness_assets.rs`
- Modify: `rust/crates/runtime/src/lib.rs` (add `pub mod harness_assets;`)
- Test: `rust/crates/runtime/tests/harness_assets.rs`

**Interfaces:**
- Produces:

```rust
pub struct HarnessAssets {
    pub skills: Vec<SkillMeta>,      // name, description, path
    pub commands: Vec<CommandMeta>,  // name, description, path, argument_hint
    pub agents: Vec<AgentMeta>,      // name, description, role kind, path
}
pub struct SkillMeta { pub name: String, pub description: String, pub path: PathBuf }
// CommandMeta / AgentMeta analogous; AgentMeta adds `pub role: AgentRole`
// where AgentRole = Implementer | Reviewer | Gate (parsed from optional `role:` frontmatter, default Implementer)

pub fn discover(cwd: &Path) -> HarnessAssets  // scans .claw/{skills,commands,agents}/**.md
                                              // + $CLAW_CONFIG_HOME equivalents; project shadows user on name clash
```

- Frontmatter format identical to the existing skill frontmatter parser (`parse_skill_frontmatter_value` in `tools/src/lib.rs:4409`) — **move/duplicate that parser here as the canonical one** and have `tools` call it, so there is one frontmatter implementation (DRY).
- Consumes: existing skill lookup-root logic; `discover` must reuse the same precedence order as `skill_lookup_roots()`.

- [ ] **Step 1: Write failing tests** — temp dir with `.claw/skills/deploy-sop/SKILL.md`, `.claw/commands/start-work.md`, `.claw/agents/qas.md` (with `role: gate`); assert all three discovered with correct names/descriptions; assert project-level shadows user-level for same name; assert malformed frontmatter yields a skip + collectable warning, not a panic.
- [ ] **Step 2: Run** `cargo test -p runtime --test harness_assets` → FAIL.
- [ ] **Step 3: Implement** `discover()` with a bounded walk (reuse the workspace-escape guards from `file_ops.rs` — no symlink escapes, no `..`).
- [ ] **Step 4: Run** → PASS; clippy clean.
- [ ] **Step 5: Commit** `feat(runtime): .claw harness asset discovery (skills/commands/agents)`

### Task 5: Skill index in the system prompt (model-invoked layer)

**Files:**
- Modify: `rust/crates/runtime/src/prompt.rs`
- Test: inline tests in `prompt.rs`

**Interfaces:**
- Consumes: `harness_assets::discover` (Task 4).
- Produces: system prompt section:

```
# Available skills
Invoke with the Skill tool before acting when a task matches:
- deploy-sop: How to deploy this project safely
- testing-patterns: Project test conventions and fixtures
```

  Cap at 50 skills / 4 KB; truncate with a "run /skills for full list" note.

- [ ] **Step 1: Write failing test** — given two `SkillMeta`s, rendered prompt contains both name+description lines and the invocation instruction.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — add `render_skill_index(&[SkillMeta]) -> Option<String>` and splice into the system prompt assembly next to where subagent/tool guidance already renders (grep `subagent` in `prompt.rs` for the insertion point).
- [ ] **Step 4: Run** `cargo test -p runtime prompt` → PASS.
- [ ] **Step 5: Commit** `feat(runtime): surface discovered skills in system prompt`

This closes SAW's core "skills auto-trigger" behavior: the model can only invoke what it knows exists.

### Task 6: User-defined markdown slash commands

**Files:**
- Modify: `rust/crates/commands/src/lib.rs` (parser + `Command` enum)
- Test: `rust/crates/commands/tests/custom_commands.rs`

**Interfaces:**
- Consumes: `CommandMeta` list (Task 4) passed into the command parser at construction.
- Produces: `Command::Custom { name: String, body: String }` — `body` is the markdown file content with `$ARGUMENTS` substituted, dispatched into the agent loop as a user turn (same execution model as Claude Code custom commands and SAW's `/start-work`, `/pre-pr` etc.). Built-in enum commands always win name conflicts; conflicting custom command is skipped with a warning.
- `/help` lists custom commands under a "Project commands" section with their frontmatter descriptions.

- [ ] **Step 1: Write failing tests** — `parse("/start-work FOO-123")` with a registered custom command returns `Command::Custom` with `$ARGUMENTS` → `FOO-123` substituted; unknown name still errors; builtin `/help` unaffected.
- [ ] **Step 2: Run** `cargo test -p commands --test custom_commands` → FAIL.
- [ ] **Step 3: Implement** — thread a `Vec<CommandMeta>` registry into the parse entry point; add the variant; wire dispatch in `main.rs` (send body as prompt).
- [ ] **Step 4: Run** → PASS across `cargo test -p commands -p rusty-claude-cli`.
- [ ] **Step 5: Commit** `feat(commands): user-defined markdown slash commands from .claw/commands`

---

## Phase 3 — Workflow Gates (SAW's stop-the-line + QAS layer)

### Task 7: Workflow state machine

**Files:**
- Create: `rust/crates/runtime/src/workflow.rs`
- Modify: `rust/crates/runtime/src/lib.rs`
- Test: `rust/crates/runtime/tests/workflow_gates.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase { Idle, Spec, Implement, Verify, Review, Done }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub phase: WorkflowPhase,
    pub task_ref: Option<String>,          // ticket/branch id
    pub acceptance_criteria: Vec<String>,  // required to leave Spec
    pub evidence: Vec<GateEvidence>,       // e.g. test-run output refs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateEvidence { pub gate: WorkflowPhase, pub kind: String, pub detail: String }

pub enum GateCheck { Pass, Blocked { reason: String } }

impl WorkflowState {
    pub fn try_advance(&mut self) -> GateCheck;  // enforces gate rules below
    pub fn record_evidence(&mut self, e: GateEvidence);
}
```

- Gate rules (SAW's immutable gates, simplified to what a single CLI can verify):
  - `Spec → Implement`: blocked unless `acceptance_criteria` non-empty (**Stop-the-Line**: no invented ACs — the model must ask the user).
  - `Implement → Verify`: always allowed.
  - `Verify → Review`: blocked unless evidence of kind `"test_run"` with a passing detail exists (**QAS gate**).
  - `Review → Done`: blocked unless evidence of kind `"review"` exists (**review gate**; human merge stays outside the CLI).
- Persisted in the existing session store (`session.rs`) so `/resume` restores phase.

- [ ] **Step 1: Write failing tests** — one test per gate rule (advance blocked without AC; passes with AC; Verify→Review blocked without test evidence; full happy path Spec→Done), plus serde round-trip.
- [ ] **Step 2: Run** `cargo test -p runtime --test workflow_gates` → FAIL.
- [ ] **Step 3: Implement** `workflow.rs` exactly per the interface.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(runtime): workflow state machine with spec/QA/review gates`

### Task 8: `/workflow` command + config toggle

**Files:**
- Modify: `rust/crates/commands/src/lib.rs` (builtin `Command::Workflow { action }`)
- Modify: `rust/crates/runtime/src/config.rs` (`#[serde(default)] workflow_gates: WorkflowGateMode` where `WorkflowGateMode = Off | Advisory | Enforced`, default `Off`)
- Modify: `rust/crates/rusty-claude-cli/src/main.rs` (dispatch)
- Test: extend `custom_commands.rs` + config tests

**Interfaces:**
- `/workflow status` prints phase, task ref, ACs, evidence. `/workflow start <task-ref>` → `Spec`. `/workflow advance` calls `try_advance()` and prints `GateCheck` result. `/workflow gate <kind> <detail>` records evidence (used by hooks/model to log a test run).
- Consumes: `WorkflowState` (Task 7).

- [ ] **Step 1: Write failing parse tests** for the four subcommands and the config enum default (`Off`).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** parse + dispatch + rendering.
- [ ] **Step 4: Run** `cargo test -p commands -p runtime` → PASS.
- [ ] **Step 5: Commit** `feat: /workflow command and workflow_gates config mode`

### Task 9: Gate enforcement in the agent loop

**Files:**
- Modify: `rust/crates/runtime/src/policy_engine.rs` (add `PolicyCondition::WorkflowPhaseIs(WorkflowPhase)` and `PolicyAction::BlockWithReason(String)` if not already expressible via existing variants — check existing variants first and reuse)
- Modify: `rust/crates/rusty-claude-cli/src/main.rs`
- Modify: `rust/crates/runtime/src/prompt.rs` (phase banner)
- Test: `rust/crates/runtime/tests/workflow_gates.rs` (extend)

**Interfaces & behavior (mode `Enforced`):**
- PreToolUse: file-writing tools (`Edit`/`Write`/`Bash`) while `phase == Spec` → Block with reason "Stop-the-line: acceptance criteria not confirmed; record them via /workflow or ask the user." (implemented as a built-in `PolicyRule`, evaluated in the same place hook `PreToolUse` decisions merge).
- Stop event: while `phase == Verify` without `test_run` evidence → `HookDecision::Block` reason "QAS gate: run the test suite and record evidence before finishing." (uses Task 3's Stop re-prompt, inherits its 3-strike cap).
- Mode `Advisory`: same triggers, but emit a warning line + `additional_context` instead of blocking.
- System prompt gains one line: `Workflow phase: implement — gates: enforced` so the model self-routes.
- Every gate decision emits a `gate_check` NDJSON event via `lane_events.rs` (evidence-based delivery / auditability).

- [ ] **Step 1: Write failing tests** — enforced mode blocks a Write tool call in Spec phase; advisory mode allows but returns context; NDJSON event emitted with `schema` field consistent with `report_schema.rs`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — prefer expressing gates as `PolicyRule`s so future rules are data, not code.
- [ ] **Step 4: Run** `cargo test --workspace` → PASS.
- [ ] **Step 5: Commit** `feat: enforce workflow gates in agent loop (stop-the-line + QAS)`

---

## Phase 4 — Role Agents (mao integration)

### Task 10: File-defined agent personas

**Files:**
- Modify: `rust/crates/rusty-claude-cli/src/mao.rs`
- Test: inline tests in `mao.rs` + fixture files under `rust/crates/rusty-claude-cli/tests/fixtures/agents/`

**Interfaces:**
- Consumes: `AgentMeta` (Task 4).
- Produces: `load_personas(assets: &HarnessAssets) -> Vec<Persona>` where `Persona { name, system_prompt, role: AgentRole }`. Hardcoded frontend/backend/database/generic personas become fallback defaults when no `.claw/agents/` exists. The Manager decomposition prompt lists available persona names dynamically instead of the hardcoded `"frontend" | "backend" | ...` union.

- [ ] **Step 1: Write failing test** — given an `AgentMeta` fixture (`qas.md` with `role: gate`), `load_personas` returns it plus defaults; manager prompt contains its name.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p rusty-claude-cli mao` → PASS.
- [ ] **Step 5: Commit** `feat(mao): load agent personas from .claw/agents with role kinds`

### Task 11: Reviewer gate pass in mao aggregation

**Files:**
- Modify: `rust/crates/rusty-claude-cli/src/mao.rs`
- Test: inline tests using the existing mock provider (`mock-anthropic-service` patterns / `mock_parity_scenarios.json` style)

**Interfaces & behavior:**
- After subagent results aggregate, if any persona with `role: Gate` exists, run one extra pass: gate persona receives the aggregated output + original briefs and returns JSON `{"verdict":"pass"|"block","findings":[...]}`. On `block`, findings are routed back to the owning subagent for **one** iteration (SAW's QAS iteration capability), then the final answer includes the verdict either way. Malformed gate JSON → treat as pass with a logged warning (gate must not brick the loop).

- [ ] **Step 1: Write failing test** with mock provider scripted to return a `block` verdict then a fixed second-pass result; assert one re-dispatch happened and final output contains the verdict.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; full `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** `feat(mao): QAS-style gate review pass with single iteration loop`

---

## Phase 5 — Bootstrap & Docs

### Task 12: `clawcli init --harness` starter pack

**Files:**
- Modify: `rust/crates/rusty-claude-cli/src/init.rs`
- Create (as embedded `include_str!` templates in a new `rust/crates/rusty-claude-cli/src/harness_templates/` dir):
  - `agents/qas.md` (`role: gate`, verification-focused system prompt)
  - `commands/start-work.md` (asks for task ref + acceptance criteria, runs `/workflow start`, confirms ACs with user — SAW `/start-work`)
  - `commands/pre-pr.md` (runs project tests via Bash, records `/workflow gate test_run`, summarizes diff — SAW `/pre-pr`)
  - `commands/end-work.md` (checks uncommitted changes, prompts commit, `/workflow advance` — SAW `/end-work`)
  - `skills/pattern-discovery/SKILL.md` ("Search First, Reuse Always, Create Only When Necessary" — grep/glob before writing new code)
  - `skills/verification-before-completion/SKILL.md` (evidence before claims: run the command, paste output, then claim)
  - `hooks-config.json` snippet merged into `.claw.json`: `session_start` → print workflow guidance; `stop` gate handled natively by Task 9 (no script needed)
- Test: `rust/crates/rusty-claude-cli/tests/init_harness.rs`

- [ ] **Step 1: Write failing test** — `init --harness` in temp dir creates the tree above, is idempotent (second run doesn't clobber user edits), and never overwrites an existing file.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** with `include_str!` templates; write-if-absent only.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(init): --harness scaffolds SAW-lite starter pack in .claw/`

### Task 13: Doctor checks + documentation

**Files:**
- Modify: doctor implementation (grep `Doctor` dispatch from `commands/src/lib.rs:1250` into `main.rs`) — add harness section: counts of discovered skills/commands/agents, frontmatter warnings from Task 4, workflow mode.
- Create: `docs/harness.md` — the three layers, `.claw/` layout, frontmatter reference, hook JSON protocol, gate semantics table, `workflow_gates` modes, attribution note crediting SAW (MIT, J. Scott Graham / ByBren LLC) since the workflow model derives from it.
- Modify: `USAGE.md` — short section linking to `docs/harness.md`; `PARITY.md` — note new hook events vs. Claude Code parity.
- Test: doctor integration test asserting the harness section renders with counts.

- [ ] **Step 1: Write failing doctor test.**
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement doctor section; write docs.**
- [ ] **Step 4: Run** `cargo test --workspace` + `scripts/fmt.sh --check` → PASS.
- [ ] **Step 5: Commit** `feat(doctor)+docs: harness diagnostics and documentation`

---

## Sequencing & Risk Notes

- **Dependency order:** 1→2→3 (hooks), 4→{5,6} (assets), 7→8→9 (gates; 9 also needs 3), 4→10→11 (mao), 12→13 last. Phases 1/2 are independent and parallelizable; Phase 3 needs both.
- **Biggest risk:** `main.rs` is 21k lines — Tasks 3, 8, 9 touch it. Keep each integration point to a small function call into `runtime`, never inline logic in `main.rs`.
- **Deliberately out of scope (YAGNI):** SAW's 11-role taxonomy (we ship 1 gate role + defaults; more roles are just `.claw/agents/*.md` files users add), Linear/Confluence integrations, tmux "dark factory", cross-provider `.gemini`/`.codex` mirrors, 3-stage human PR review (human merge authority already lives outside the CLI).
- **Zero-regression guarantee:** with no `.claw/` dir and `workflow_gates` unset, every new code path is dormant; existing hook config keys unchanged.
