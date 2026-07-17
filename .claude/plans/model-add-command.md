# Plan: `/model add` — interactive "add model" for the REPL

## Context / current state
- `/model` parses to `SlashCommand::Model { model: Option<String> }` in
  `rust/crates/commands/src/lib.rs:1373`.
  - `/model`          → prints the active model report (`set_model`, main.rs:9246)
  - `/model <name>`   → switches the active model for the session
  - resume-mode path  → main.rs:7484 (JSON `kind:"models"`)
- Config already supports a user-defined **aliases** map
  (`{"aliases": {"fast": "claude-haiku-4-5-20251213"}}`) read by
  `RuntimeConfig::aliases()` (config.rs:900/1008) and resolved by
  `resolve_model_alias_with_config` (main.rs:3438). So "a model you added" =
  a named alias pointing at a real `provider/model`.
- Persistence precedent: `save_user_provider_settings` (config.rs:1136) uses
  private helpers `read_settings_root`/`write_settings_root` (config.rs:1207/1219)
  and `default_config_home()` (config.rs:1126, already `pub`) writing to
  `~/.claw/settings.json` (0600 on unix).
- Interactive prompt inside a slash command is an established pattern:
  `/setup` → `setup_wizard::run_setup_wizard()` (main.rs:9077) uses
  `read_line` from stdin (setup_wizard.rs:292).

## Decision (from user)
UX = **interactive prompt**. `/model add` (no args) opens prompts asking for
alias + provider/model, validates, persists, and the new alias is immediately
usable via `/model <alias>`. Also add `/model remove <alias>` and surface
added models in the `/model` listing.

## Where work happens
All inside the isolated worktree
`CUSTOM_CLI_MODEL_ADD` on branch `feat/model-add-command`
(branched off `feat/saw-harness-integration` HEAD 51ea374).

## Changes

### 1. `rust/crates/commands/src/lib.rs` — parsing
- Extend the `"model"` parse arm (line 1373) into a small dispatcher modeled on
  `parse_mcp_command`/`parse_plugin_command` so we accept a first sub-token:
  - `/model`                → `Model { model: None }`            (unchanged)
  - `/model add`            → `Model { model: Some("add".into()) }`  (interactive)
  - `/model remove <alias>` → `ModelAdd { remove: Some(alias) }` ... **see note**
  - `/model <name>`         → `Model { model: Some(name) }`        (unchanged: switch)
  - Resolution rule for the ambiguity: treat `add` / `remove` / `list` as
    reserved subcommands ONLY when they are the sole token or followed by a
    single alias arg; any other string (incl. `add` used as a real model id
    — unlikely) falls through to the switch path. Use `parse_model_command`
    helper (new) that inspects `args`.
- Minimal, least-invasive representation: keep the existing `Model { model }`
  variant and add ONE new variant `ModelAdd { remove: Option<String> }` plus
  reuse `Model { model: None }` for `add`? **No** — cleaner to add a dedicated
  variant. Final enum additions:
  - `ModelAdd { alias: Option<String>, model: Option<String> }`
    (when both `None` → interactive; pre-filled when args given)
  - `ModelRemove { alias: Option<String> }`
  Add corresponding arms to `slash_name()` (`/model`, `/model`, `/model`).
- Update `handle_slash_command` exhaustive match (lib.rs:5361) and the
  resume/outcome matches in main.rs to handle the new variants (see §3).
- Update the `SlashCommandSpec` for `"model"` (lib.rs:90): keep summary, tweak
  `argument_hint` to `Some("[model|add|remove <alias>]")`.

### 2. `rust/crates/runtime/src/config.rs` — persistence (public API)
- Promote `read_settings_root` + `write_settings_root` to `pub(crate)` is not
  enough (CLI crate needs them). Add **three** thin `pub` functions next to
  `save_user_provider_settings` (config.rs:1136), mirroring its shape:
  - `pub fn save_user_model_alias(alias, model) -> Result<(), ConfigError>`
    — reads `~/.claw/settings.json` root, ensures `aliases` object exists,
    inserts `alias → model`, writes back, chmod 0600. Overwrites if alias
    already present (idempotent re-add).
  - `pub fn remove_user_model_alias(alias) -> Result<bool, ConfigError>`
    — removes the alias key; returns whether it existed.
  - `pub fn load_user_model_aliases() -> Result<BTreeMap<String,String>, ConfigError>`
    — convenience reader of just the `aliases` object from the user file.
  (Implementation reuses `read_settings_root`/`write_settings_root`; leave those
   private and just call them from inside the new pub fns.)
- Re-export the three new fns from `rust/crates/runtime/src/lib.rs:69`
  alongside `save_user_provider_settings`.

### 3. `rust/crates/rusty-claude-cli/src/main.rs` — REPL handling
- In the interactive slash dispatcher (line ~8995):
  - `SlashCommand::Model { model } => self.set_model(model)?` (unchanged)
  - add `SlashCommand::ModelAdd { alias, model } => self.add_model(alias, model)?,`
  - add `SlashCommand::ModelRemove { alias } => self.remove_model(alias)?,`
- New methods on the REPL struct (next to `set_model`, main.rs:9246):
  - `fn add_model(&mut self, alias: Option<String>, model: Option<String>) -> Result<bool>`
    - Interactive path (uses `read_line`, same style as setup_wizard.rs:292):
      prompt for alias if `None`; reject reserved words (`add`/`remove`/`list`
      and empty); prompt for provider/model if `None`.
    - Validate the target with the EXISTING `validate_model_syntax` (main.rs:3455)
      and `resolve_model_alias_with_config`.
    - Refuse to shadow a built-in alias (`opus`/`sonnet`/`haiku`/`gpt-oss`...) —
      check against `resolve_model_alias` table keys; print a clear message.
    - Persist via `runtime::save_user_model_alias`.
    - Print a confirmation report (alias, resolved model, file written). Do NOT
      switch the active model automatically (mirrors `--model` being opt-in).
    - Return `Ok(false)` (no runtime rebuild needed).
  - `fn remove_model(&mut self, alias: Option<String>) -> Result<bool>`
    - Interactive alias prompt if `None`.
    - `runtime::remove_user_model_alias`; report removed vs not-found.
    - `Ok(false)`.
- Enrich the `/model` listing (`set_model` no-arg path, main.rs:9247, and the
  resume JSON path main.rs:7484): show a "Configured aliases" section from
  `runtime::load_user_model_aliases()` (merged with built-ins for display).
  Add `configured_aliases` to the `kind:"models"` JSON object.
- Update the `/model` help topic text (main.rs:11178) and the slash spec to
  mention `add` / `remove`.
- Add the new variants to the resume-command matcher (main.rs ~7506+): the
  interactive add/remove are NOT resume-safe in non-interactive mode, so route
  them like `Setup` (interactive-only) — return a clear error in the resume
  path, OR support a fully-specified non-interactive form `/model add <a> <m>`.
  Decision: support **both** — if alias+model both given, no prompt is needed
  and it works headless; only prompt when args are missing.

### 4. Tests
- `commands/src/lib.rs` tests (mirror existing `parses_supported_slash_commands`,
  lib.rs:5561): assert
  `/model add` → `ModelAdd{None,None}`,
  `/model add fast openai/gpt-oss-20b` → `ModelAdd{Some("fast"),Some("openai/gpt-oss-20b")}`,
  `/model remove fast` → `ModelRemove{Some("fast")}`,
  and that `/model add` still allows `/model opus` to mean switch (not
  interpreted as alias "opus").
- `runtime/src/config.rs` tests (mirror
  `parses_user_defined_model_aliases_from_settings`, config.rs:3619):
  `save_user_model_alias` writes+reloads correctly, overwrites, and
  `remove_user_model_alias` returns the right bool.
- `rusty-claude-cli` integration: a headless test invoking
  `/model add <alias> <model>` via the resume/one-shot path (no stdin needed
  when both args supplied) asserting the alias lands in a temp settings file
  and is resolvable — mirror `output_format_contract.rs` style.

### 5. Docs
- `USAGE.md` / `docs`: add a short "/model add" section near the existing
  `/model` guidance (USAGE.md:49+).

## Compile/test gate
- `cargo build -p commands -p runtime` then `cargo test -p commands -p runtime`
  for the fast unit tests; full `cargo test -p rusty-claude-cli` for the
  integration test (slower; run if time permits).
- Fix all match-exhaustiveness errors the new variants introduce (the main
  risk surface — there are several exhaustive matches on `SlashCommand`).

## Merge
- Work happens entirely in worktree `CUSTOM_CLI_MODEL_ADD`.
- When green: commit, then merge `feat/model-add-command` into
  `feat/saw-harness-integration` (current working branch of the main repo) —
  NOT `main` — to avoid disturbing other agents. Use `git merge --no-ff`.
- Leave the worktree in place until merge succeeds, then clean it up.
