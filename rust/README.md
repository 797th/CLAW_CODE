# 🦞 Claw Code — Rust Implementation

A high-performance Rust rewrite of the Claw Code CLI agent harness. Built for speed, safety, and native tool execution.

For a task-oriented guide with copy/paste examples, see [`../USAGE.md`](../USAGE.md).

## Quick Start

```bash
# Inspect available commands
cd rust/
cargo run -p rusty-claude-cli --bin clawcli -- --help

# Build the workspace
cargo build --workspace

# Run the full-screen TUI (this is the default for `clawcli`)
cargo run -p rusty-claude-cli --bin clawcli --

# Use the legacy line-based REPL when needed
cargo run -p rusty-claude-cli --bin clawcli -- --repl

# Run the interactive REPL
cargo run -p rusty-claude-cli --bin clawcli -- --model openai/gpt-oss-120b

# One-shot prompt
cargo run -p rusty-claude-cli --bin clawcli -- prompt "explain this codebase"

# JSON output for automation
cargo run -p rusty-claude-cli --bin clawcli -- --output-format json prompt "summarize src/main.rs"

# The convenience launcher (same source, relaxed CWD guard)
cargo run -p rusty-claude-cli --bin cliclaw --
```

The workspace emits three entrypoints from the same `main.rs`:

- `clawcli`: canonical binary name used by the docs and tests
- `claw`: compatibility launcher
- `cliclaw`: convenience launcher — same binary under a name that relaxes the working-directory guard (handy for global, run-from-anywhere installs)

To install **both** binaries on macOS, Linux, or Windows, use the repo-root installer (see `../README.md`):

```bash
python3 ../install/install.py        # builds + installs clawcli and cliclaw, updates PATH
```

Older `cli797` installs still get the same launcher defaults if you already have that binary lying around.

## Configuration

On the first interactive run, `clawcli` opens a setup wizard when no
credentials are found. Choose OpenAI-compatible or Anthropic-compatible,
enter a base URL, API key, and model. The OpenAI-compatible option defaults to
`https://api.openai.com/v1`, so the same flow supports custom gateways and
local OpenAI-compatible servers.

For non-interactive setup, set your API credentials:

```bash
export OPENAI_BASE_URL="https://your-gateway.example/v1"
export OPENAI_API_KEY="..."
```

Anthropic-compatible endpoints still work too:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# Or use a proxy:
export ANTHROPIC_BASE_URL="https://your-proxy.com"
```

The wizard stores credentials in `~/.claw/.env` (or
`$CLAW_CONFIG_HOME/.env`) and the selected model in `settings.json`.

For local OpenAI-compatible servers such as Ollama, including Qwen reasoning
models, see [`../docs/local-openai-compatible-providers.md`](../docs/local-openai-compatible-providers.md).
Use the exact model tag exposed by the server, for example `qwen3:latest`, and
prefer `OLLAMA_HOST` for Ollama-specific local routing.

## Mock parity harness

The workspace now includes a deterministic Anthropic-compatible mock service and a clean-environment CLI harness for end-to-end parity checks.

```bash
cd rust/

# Run the scripted clean-environment harness
./scripts/run_mock_parity_harness.sh

# Or start the mock service manually for ad hoc CLI runs
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0
```

Harness coverage:

- `streaming_text`
- `read_file_roundtrip`
- `grep_chunk_assembly`
- `write_file_allowed`
- `write_file_denied`
- `multi_tool_turn_roundtrip`
- `bash_stdout_roundtrip`
- `bash_permission_prompt_approved`
- `bash_permission_prompt_denied`
- `plugin_tool_roundtrip`

Primary artifacts:

- `crates/mock-anthropic-service/` — reusable mock Anthropic-compatible service
- `crates/rusty-claude-cli/tests/mock_parity_harness.rs` — clean-env CLI harness
- `scripts/run_mock_parity_harness.sh` — reproducible wrapper
- `scripts/run_mock_parity_diff.py` — scenario checklist + PARITY mapping runner
- `mock_parity_scenarios.json` — scenario-to-PARITY manifest

## Features

| Feature | Status |
|---------|--------|
| Anthropic / OpenAI-compatible provider flows + streaming | ✅ |
| Direct bearer-token auth via `ANTHROPIC_AUTH_TOKEN` | ✅ |
| Interactive REPL (rustyline) | ✅ |
| Tool system (bash, read, write, edit, grep, glob) | ✅ |
| Web tools (search, fetch) | ✅ |
| Sub-agent / agent surfaces | ✅ |
| Todo tracking | ✅ |
| Notebook editing | ✅ |
| CLAUDE.md / CLAW.md / AGENTS.md project memory | ✅ |
| Config file hierarchy (`.claw.json` + merged config sections) | ✅ |
| Permission system | ✅ |
| MCP server lifecycle + inspection | ✅ |
| Session persistence + resume | ✅ |
| Cost / usage / stats surfaces | ✅ |
| Git integration | ✅ |
| Markdown terminal rendering (ANSI) | ✅ |
| Model aliases (opus/sonnet/haiku) | ✅ |
| Direct CLI subcommands (`status`, `sandbox`, `agents`, `mcp`, `skills`, `doctor`) | ✅ |
| Slash commands (including `/skills`, `/agents`, `/mcp`, `/doctor`, `/plugin`, `/subagent`) | ✅ |
| Hooks (`/hooks`, config-backed lifecycle hooks) | ✅ |
| Plugin management surfaces | ✅ |
| Skills inventory / install / uninstall surfaces | ✅ |
| Machine-readable JSON output across core CLI surfaces | ✅ |

## Model Aliases

Short names resolve to the latest model versions:

| Alias | Resolves To |
|-------|------------|
| `gpt-oss` | `openai/gpt-oss-120b` |
| `gpt-oss-20b` | `openai/gpt-oss-20b` |
| `opus` | `claude-opus-4-6` |
| `sonnet` | `claude-sonnet-4-6` |
| `haiku` | `claude-haiku-4-5-20251213` |

## CLI Flags and Commands

Representative current surface:

```text
clawcli [OPTIONS] [COMMAND]
cliclaw [OPTIONS] [COMMAND]

Flags:
  --model MODEL
  --output-format text|json  (case-insensitive; CLAW_OUTPUT_FORMAT supplies the default, flags override env)
  --permission-mode MODE
  --cwd PATH, -C PATH, --directory PATH
  --dangerously-skip-permissions, --skip-permissions
  --allowedTools TOOLS        canonical snake_case names or aliases; status JSON exposes allowed_tools.available/aliases
  --resume [SESSION.jsonl|session-id|latest]
  --version, -V

Top-level commands:
  prompt <text>
  help
  version
  status
  sandbox
  acp [serve]
  dump-manifests
  bootstrap-plan
  agents
  mcp
  skills
  system-prompt
  init
```

`clawcli acp` is a local discoverability surface for editor-first users: it reports the current ACP/Zed status without starting the runtime. As of April 16, 2026, claw-code does **not** ship an ACP/Zed daemon entrypoint yet, and `clawcli acp serve` is only a status alias until the real protocol surface lands.

The command surface is moving quickly. For the canonical live help text, run:

```bash
cargo run -p rusty-claude-cli --bin clawcli -- --help
```

## Slash Commands (REPL)

Tab completion expands slash commands, model aliases, permission modes, and recent session IDs.

The REPL now exposes a much broader surface than the original minimal shell:

- session / visibility: `/help`, `/status`, `/sandbox`, `/cost`, `/resume`, `/session`, `/version`, `/usage`, `/stats`
- workspace / git: `/compact`, `/clear`, `/config`, `/memory`, `/init`, `/diff`, `/commit`, `/pr`, `/issue`, `/export`, `/hooks`, `/files`, `/release-notes`
- discovery / debugging: `/mcp`, `/agents`, `/skills`, `/doctor`, `/tasks`, `/context`, `/desktop`
- automation / analysis: `/review`, `/advisor`, `/insights`, `/security-review`, `/subagent`, `/team`, `/telemetry`, `/providers`, `/cron`, and more
- plugin management: `/plugin` (with aliases `/plugins`, `/marketplace`)

Notable claw-first surfaces now available directly in slash form:
- `/skills [list|show <name>|install <path>|uninstall <name>|help]`
- `/agents [list|show <name>|create <name>|help]`
- `/mcp [list|show <server>|help]`
- `/doctor`
- `/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]`
- `/subagent [list|steer <target> <msg>|kill <id>]`

See [`../USAGE.md`](../USAGE.md) for usage examples and run `cargo run -p rusty-claude-cli --bin clawcli -- --help` for the live canonical command list.

## Workspace Layout

```text
rust/
├── Cargo.toml              # Workspace root
├── Cargo.lock
└── crates/
    ├── api/                # Provider clients + streaming + request preflight
    ├── claw-tui/           # Full-screen ratatui TUI frontend library
    ├── commands/           # Shared slash-command registry + help rendering
    ├── compat-harness/     # Compatibility/parity harness utilities
    ├── mock-anthropic-service/ # Deterministic local Anthropic-compatible mock
    ├── plugins/            # Plugin metadata, manager, install/enable/disable surfaces
    ├── runtime/            # Session, config, permissions, MCP, prompts, auth/runtime loop
    ├── rusty-claude-cli/   # Main CLI binary (`clawcli`)
    ├── telemetry/          # Session tracing and usage telemetry types
    └── tools/              # Built-in tools, skill resolution, tool search, agent runtime surfaces
```

### Crate Responsibilities

- **api** — provider clients, SSE streaming, request/response types, auth (`ANTHROPIC_API_KEY` + bearer-token support), request-size/context-window preflight
- **commands** — slash command definitions, parsing, help text generation, JSON/text command rendering
- **compat-harness** — compatibility and parity helpers for comparing behavior with upstream fixtures
- **mock-anthropic-service** — deterministic `/v1/messages` mock for CLI parity tests and local harness runs
- **plugins** — plugin metadata, install/enable/disable/update flows, plugin tool definitions, hook integration surfaces
- **runtime** — `ConversationRuntime`, config loading, session persistence, permission policy, MCP client lifecycle, system prompt assembly, usage tracking
- **rusty-claude-cli** — REPL, one-shot prompt, direct CLI subcommands, streaming display, tool call rendering, CLI argument parsing
- **telemetry** — session trace events and supporting telemetry payloads
- **tools** — tool specs + execution: Bash, ReadFile, WriteFile, EditFile, GlobSearch, GrepSearch, WebSearch, WebFetch, Agent, TodoWrite, NotebookEdit, Skill, ToolSearch, and runtime-facing tool discovery

## Stats

- **~20K lines** of Rust
- **10 crates** in workspace
- **Binary names:** `clawcli`, `claw`, `cliclaw`
- **Default model:** `openai/gpt-oss-120b`
- **Default permissions:** `danger-full-access`

## License

See repository root.
