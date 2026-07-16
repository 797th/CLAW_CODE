# Claw Code Usage

This guide covers the current Rust workspace under `rust/` and the `clawcli` CLI binary. If you are brand new, make the doctor health check your first run: start `clawcli`, then run `/doctor`.

## Quick-start health check

Run this before prompts, sessions, or automation:

```bash
cd rust
cargo build --workspace
./target/debug/clawcli
# first command inside the REPL
/doctor
```

`/doctor` is the built-in setup and preflight diagnostic. Once you have a saved session, you can rerun it with `./target/debug/clawcli --resume latest /doctor`.

## Prerequisites

- Rust toolchain with `cargo`
- One of:
  - `OPENAI_API_KEY` for any OpenAI-compatible endpoint
  - `ANTHROPIC_API_KEY` for direct API access
  - `ANTHROPIC_AUTH_TOKEN` for bearer-token auth
- Optional: `OPENAI_BASE_URL` when targeting a non-default OpenAI-compatible service
- Optional: `ANTHROPIC_BASE_URL` when targeting a proxy or local service

## Install / build the workspace

### One command, every OS (recommended)

```bash
python3 install/install.py            # debug build (default)
python3 install/install.py --release  # optimized build
```

This works identically on macOS, Linux, and Windows. It builds both `clawcli` and `cliclaw`, copies them into a bin directory (`$CARGO_HOME/bin`, `~/.cargo/bin`, or `~/.local/bin` on Unix; the equivalent on Windows), and adds that directory to your PATH. Open a new terminal afterward so the PATH change takes effect. Prerequisites: Python 3 and a Rust toolchain (`cargo` + `rustc`) on PATH. See `README.md` for details and the `--install-dir` / `--no-path-update` flags.

### Manual build

```bash
cd rust
cargo build --workspace
```

The CLI binary is available at `rust/target/debug/clawcli` after a debug build. Make the doctor check above your first post-build step.

On the first interactive run, if the selected model has no credentials,
`clawcli` opens a connection setup wizard. It defaults to an OpenAI-compatible
connection and asks for a base URL, API key, and model. The same flow works for
OpenAI, OpenRouter, Ollama, vLLM, LM Studio, or another service that implements
the OpenAI chat-completions API. Credentials are stored in the user
`.claw/.env` file and the model in `~/.claw/settings.json` (or the path selected
by `CLAW_CONFIG_HOME`).

For non-interactive setup, set the standard variables directly:

```bash
export OPENAI_BASE_URL="https://your-gateway.example/v1"
export OPENAI_API_KEY="..."
```

The default model fallback remains `openai/gpt-oss-120b`; set `OPENAI_MODEL` or
the `model` setting when your endpoint exposes a different model.

The installer builds **both** `clawcli` and `cliclaw` on every OS. On Windows these are `clawcli.exe` and `cliclaw.exe`. They share one `main.rs`; the filename changes behavior at runtime — when launched as `cliclaw`, the CLI keeps the directory you launched from as the active workspace, defaults to `danger-full-access`, and allows broad-CWD runs.

## Global launcher (`cliclaw`)

If you want a Claude Code-style global command from any terminal window, the installer already provides it:

```bash
python3 install/install.py        # builds + installs both clawcli and cliclaw
cliclaw                           # now available from any directory, any OS
```

The installer copies `cliclaw` into a user-level bin directory and adds that directory to your PATH when needed. After installation, running `cliclaw` from `C:\some\project` (Windows) or any folder (macOS/Linux) starts the CLI right there.

## Quick start

### First-run doctor check

```bash
cd rust
./target/debug/clawcli
/doctor
```

Or run doctor directly with JSON output for scripting:

```bash
cd rust
./target/debug/clawcli doctor --output-format json
```

**Note:** Diagnostic verbs (`doctor`, `status`, `sandbox`, `version`) support `--output-format json` for machine-readable output. Invalid suffix arguments (e.g., `--json`) are now rejected at parse time rather than falling through to prompt dispatch.

### Initialize a repository

Set up a new repository with `.claw` config, `.claw.json`, `.gitignore` entries, and a `CLAUDE.md` guidance file:

```bash
cd /path/to/your/repo
./target/debug/clawcli init
```

Text mode (human-readable) shows artifact creation summary with project path and next steps. Idempotent — running multiple times in the same repo marks already-created files as "skipped".

JSON mode for scripting:
```bash
./target/debug/clawcli init --output-format json
```

Returns structured output with `project_path`, `created[]`, `updated[]`, `skipped[]` arrays (one per artifact), and `artifacts[]` carrying each file's `name` and machine-stable `status` tag. The legacy `message` field preserves backward compatibility.

**Why structured fields matter:** Claws can detect per-artifact state (`created` vs `updated` vs `skipped`) without substring-matching human prose. Use the `created[]`, `updated[]`, and `skipped[]` arrays for conditional follow-up logic (e.g., only commit if files were actually created, not just updated).

### Interactive REPL

```bash
cd rust
./target/debug/clawcli
```

### One-shot prompt

```bash
cd rust
./target/debug/clawcli prompt "summarize this repository"
```

### Shorthand prompt mode

```bash
cd rust
./target/debug/clawcli "explain rust/crates/runtime/src/lib.rs"
```

### JSON output for scripting

```bash
cd rust
./target/debug/clawcli --output-format json prompt "status"
```

### Inspect worker state

The `clawcli state` command reads `.claw/worker-state.json`, which is written by the interactive REPL or a one-shot prompt when a worker executes a task. This file contains the worker ID, session reference, model, and permission mode.

Prerequisite: You must run `clawcli` (interactive REPL) or `clawcli prompt <text>` at least once in the repository to produce the worker state file.

```bash
cd rust
./target/debug/clawcli state
```

JSON mode:
```bash
./target/debug/clawcli state --output-format json
```

If you run `clawcli state` before any worker has executed, you will see a helpful error:
```
error: no worker state file found at .claw/worker-state.json
  Hint: worker state is written by the interactive REPL or a non-interactive prompt.
  Run:   clawcli               # start the REPL (writes state on first turn)
  Or:    clawcli prompt <text> # run one non-interactive turn
  Then rerun: clawcli state [--output-format json]
```

## Advanced slash commands (Interactive REPL only)

These commands are available inside the interactive REPL (`clawcli` with no args). They extend the assistant with workspace analysis, planning, and navigation features.

### `/ultraplan` — Deep planning with multi-step reasoning

**Purpose:** Break down a complex task into steps using extended reasoning.

```bash
# Start the REPL
clawcli

# Inside the REPL
/ultraplan refactor the auth module to use async/await
/ultraplan design a caching layer for database queries
/ultraplan analyze this module for performance bottlenecks
```

Output: A structured plan with numbered steps, reasoning for each step, and expected outcomes. Use this when you want the assistant to think through a problem in detail before coding.

### `/teleport` — Jump to a file or symbol

**Purpose:** Quickly navigate to a file, function, class, or struct by name.

```bash
# Jump to a symbol
/teleport UserService
/teleport authenticate_user
/teleport RequestHandler

# Jump to a file
/teleport src/auth.rs
/teleport crates/runtime/lib.rs
/teleport ./ARCHITECTURE.md
```

Output: The file content, with the requested symbol highlighted or the file fully loaded. Useful for exploring the codebase without manually navigating directories. If multiple matches exist, the assistant shows the top candidates.

### `/bughunter` — Scan for likely bugs and issues

**Purpose:** Analyze code for common pitfalls, anti-patterns, and potential bugs.

```bash
# Scan the entire workspace
/bughunter

# Scan a specific directory or file
/bughunter src/handlers
/bughunter rust/crates/runtime
/bughunter src/auth.rs
```

Output: A list of suspicious patterns with explanations (e.g., "unchecked unwrap()", "potential race condition", "missing error handling"). Each finding includes the file, line number, and suggested fix. Use this as a first pass before a full code review.

## Model and permission controls

```bash
cd rust
./target/debug/clawcli --model gpt-oss-20b prompt "review this diff"
./target/debug/clawcli --permission-mode read-only prompt "summarize Cargo.toml"
./target/debug/clawcli --permission-mode workspace-write prompt "update README.md"
./target/debug/clawcli --allowedTools read,glob "inspect the runtime crate"
```

`cliclaw` bakes in the broad/full-access defaults already, so launching through `cliclaw` (any OS) behaves like:

```bash
cliclaw
cliclaw prompt "review this repository"
```

Older `cli797` binaries still map to the same permissive launcher defaults for compatibility.

Supported permission modes:

- `read-only`
- `workspace-write`
- `danger-full-access`

Model aliases currently supported by the CLI:

- `gpt-oss` -> `openai/gpt-oss-120b`
- `gpt-oss-20b` -> `openai/gpt-oss-20b`
- `opus` → `claude-opus-4-6`
- `sonnet` → `claude-sonnet-4-6`
- `haiku` → `claude-haiku-4-5-20251213`

## Authentication

### NVIDIA NIM hosted endpoint

```bash
export OPENAI_BASE_URL="https://integrate.api.nvidia.com/v1"
export OPENAI_API_KEY="nvapi-..."

cd rust
./target/debug/clawcli prompt "reply with the word ready"
./target/debug/clawcli --model gpt-oss-20b prompt "reply with the word ready"
```

### Anthropic API key

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

### OAuth

```bash
cd rust
export ANTHROPIC_AUTH_TOKEN="anthropic-oauth-or-proxy-bearer-token"
```

### Which env var goes where

`clawcli` accepts generic OpenAI-compatible connections plus two Anthropic credential env vars. The Anthropic vars are **not interchangeable** — the HTTP header Anthropic expects differs per credential shape. Putting the wrong value in the wrong slot is the most common 401 we see.

| Credential shape | Env var | HTTP header | Typical source |
|---|---|---|---|
| NVIDIA NIM / NVIDIA hosted OpenAI-compatible key | `OPENAI_API_KEY` + `OPENAI_BASE_URL=https://integrate.api.nvidia.com/v1` | `Authorization: Bearer ...` | [build.nvidia.com](https://build.nvidia.com) |
| `sk-ant-*` API key | `ANTHROPIC_API_KEY` | `x-api-key: sk-ant-...` | [console.anthropic.com](https://console.anthropic.com) |
| OAuth access token (opaque) | `ANTHROPIC_AUTH_TOKEN` | `Authorization: Bearer ...` | an Anthropic-compatible proxy or OAuth flow that mints bearer tokens |
| OpenRouter key (`sk-or-v1-*`) | `OPENAI_API_KEY` + `OPENAI_BASE_URL=https://openrouter.ai/api/v1` | `Authorization: Bearer ...` | [openrouter.ai/keys](https://openrouter.ai/keys) |

**Why this matters:** if you paste an `sk-ant-*` key into `ANTHROPIC_AUTH_TOKEN`, Anthropic's API will return `401 Invalid bearer token` because `sk-ant-*` keys are rejected over the Bearer header. The fix is a one-line env var swap — move the key to `ANTHROPIC_API_KEY`. Recent `clawcli` builds detect this exact shape (401 + `sk-ant-*` in the Bearer slot) and append a hint to the error message pointing at the fix.

**If you meant a different provider:** if `clawcli` reports missing Anthropic credentials but you already have `OPENAI_API_KEY`, `XAI_API_KEY`, or `DASHSCOPE_API_KEY` exported, you most likely forgot to prefix the model name with the provider's routing prefix. Use `--model openai/gpt-4.1-mini` (OpenAI-compat / OpenRouter / Ollama), `--model grok` (xAI), or `--model qwen-plus` (DashScope) and the prefix router will select the right backend regardless of the ambient credentials. The error message now includes a hint that names the detected env var.

## Local Models

`clawcli` can talk to local servers and provider gateways through either Anthropic-compatible or OpenAI-compatible endpoints. The first-run wizard asks for the same URL/key pair interactively. For scripts, use `ANTHROPIC_BASE_URL` with `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`, or `OPENAI_BASE_URL` with `OPENAI_API_KEY`.

The user-level `.claw/.env` file is also supported for these variables, so
credentials do not need to be exported in every shell. `CLAW_ENDPOINT_TYPE`
(`openai-compatible` or `anthropic-compatible`) records the selected protocol
when the model name itself is ambiguous.

### Anthropic-compatible endpoint

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_AUTH_TOKEN="local-dev-token"

cd rust
./target/debug/clawcli --model "claude-sonnet-4-6" prompt "reply with the word ready"
```

### OpenAI-compatible endpoint

```bash
export OPENAI_BASE_URL="http://127.0.0.1:8000/v1"
export OPENAI_API_KEY="local-dev-token"

cd rust
./target/debug/clawcli --model "qwen2.5-coder" prompt "reply with the word ready"
```

### NVIDIA NIM hosted GPT-OSS

```bash
export OPENAI_BASE_URL="https://integrate.api.nvidia.com/v1"
export OPENAI_API_KEY="nvapi-..."

cd rust
./target/debug/clawcli --model "gpt-oss" prompt "summarize this repository in one sentence"
./target/debug/clawcli --model "gpt-oss-20b" prompt "summarize this repository in one sentence"
```

### Ollama

```bash
export OPENAI_BASE_URL="http://127.0.0.1:11434/v1"
unset OPENAI_API_KEY

cd rust
./target/debug/clawcli --model "llama3.2" prompt "summarize this repository in one sentence"
```

### OpenRouter

```bash
export OPENAI_BASE_URL="https://openrouter.ai/api/v1"
export OPENAI_API_KEY="sk-or-v1-..."

cd rust
./target/debug/clawcli --model "openai/gpt-4.1-mini" prompt "summarize this repository in one sentence"
```

### Alibaba DashScope (Qwen)

For Qwen models via Alibaba's native DashScope API (higher rate limits than OpenRouter):

```bash
export DASHSCOPE_API_KEY="sk-..."

cd rust
./target/debug/clawcli --model "qwen/qwen-max" prompt "hello"
# or bare:
./target/debug/clawcli --model "qwen-plus" prompt "hello"
```

Model names starting with `qwen/` or `qwen-` are automatically routed to the DashScope compatible-mode endpoint (`https://dashscope.aliyuncs.com/compatible-mode/v1`). You do **not** need to set `OPENAI_BASE_URL` or unset `ANTHROPIC_API_KEY` — the model prefix wins over the ambient credential sniffer.

Reasoning variants (`qwen-qwq-*`, `qwq-*`, `*-thinking`) automatically strip `temperature`/`top_p`/`frequency_penalty`/`presence_penalty` before the request hits the wire (these params are rejected by reasoning models).

## Supported Providers & Models

`clawcli` has protocol backends for Anthropic Messages and OpenAI-compatible
Chat Completions, plus built-in routing presets for several hosted providers.
The provider is selected automatically based on an explicit endpoint type or
model name, falling back to the configured credential.

### Provider matrix

| Provider | Protocol | Auth env var(s) | Base URL env var | Default base URL |
|---|---|---|---|---|
| **Anthropic** (direct) | Anthropic Messages API | `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` | `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` |
| **xAI** | OpenAI-compatible | `XAI_API_KEY` | `XAI_BASE_URL` | `https://api.x.ai/v1` |
| **OpenAI-compatible** | OpenAI Chat Completions | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `https://api.openai.com/v1` |
| **DashScope** (Alibaba) | OpenAI-compatible | `DASHSCOPE_API_KEY` | `DASHSCOPE_BASE_URL` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |

The OpenAI-compatible backend also serves as the gateway for **OpenRouter**, **Ollama**, and any other service that speaks the OpenAI `/v1/chat/completions` wire format — just point `OPENAI_BASE_URL` at the service.

**Model-name prefix routing:** If a model name starts with `openai/`, `gpt-`, `qwen/`, or `qwen-`, the provider is selected by the prefix regardless of which env vars are set. This prevents accidental misrouting to Anthropic when multiple credentials exist in the environment.

### Tested models and aliases

These are the models registered in the built-in alias table with known token limits:

| Alias | Resolved model name | Provider | Max output tokens | Context window |
|---|---|---|---|---|
| `gpt-oss` | `openai/gpt-oss-120b` | OpenAI-compatible / NVIDIA NIM | 8 192 | â€” |
| `gpt-oss-20b` | `openai/gpt-oss-20b` | OpenAI-compatible / NVIDIA NIM | 8 192 | â€” |
| `opus` | `claude-opus-4-6` | Anthropic | 32 000 | 200 000 |
| `sonnet` | `claude-sonnet-4-6` | Anthropic | 64 000 | 200 000 |
| `haiku` | `claude-haiku-4-5-20251213` | Anthropic | 64 000 | 200 000 |
| `grok` / `grok-3` | `grok-3` | xAI | 64 000 | 131 072 |
| `grok-mini` / `grok-3-mini` | `grok-3-mini` | xAI | 64 000 | 131 072 |
| `grok-2` | `grok-2` | xAI | — | — |

Any model name that does not match an alias is passed through verbatim. This is how you use OpenRouter model slugs (`openai/gpt-4.1-mini`), Ollama tags (`llama3.2`), or full Anthropic model IDs (`claude-sonnet-4-20250514`).

### User-defined aliases

You can add custom aliases in any settings file (`~/.claw/settings.json`, `.claw/settings.json`, or `.claw/settings.local.json`):

```json
{
  "aliases": {
    "fast": "claude-haiku-4-5-20251213",
    "smart": "claude-opus-4-6",
    "cheap": "grok-3-mini"
  }
}
```

Local project settings override user-level settings. Aliases resolve through the built-in table, so `"fast": "haiku"` also works.

### How provider detection works

1. An explicit `CLAW_ENDPOINT_TYPE` (`openai-compatible` or `anthropic-compatible`) wins.
2. Otherwise, known model names select their built-in routing preset.
3. Otherwise, a configured `OPENAI_BASE_URL`/`OPENAI_API_KEY` pair selects the generic OpenAI-compatible backend.
4. If nothing matches, it falls back to the configured credential and then Anthropic.

## FAQ

### What about Codex?

The name "codex" appears in the Claw Code ecosystem but it does **not** refer to OpenAI Codex (the code-generation model). Here is what it means in this project:

- **`oh-my-codex` (OmX)** is the workflow and plugin layer that sits on top of `clawcli`. It provides planning modes, parallel multi-agent execution, notification routing, and other automation features. See [PHILOSOPHY.md](./PHILOSOPHY.md) and the [oh-my-codex repo](https://github.com/Yeachan-Heo/oh-my-codex).
- **`.codex/` directories** (e.g. `.codex/skills`, `.codex/agents`, `.codex/commands`) are legacy lookup paths that `clawcli` still scans alongside the primary `.claw/` directories.
- **`CODEX_HOME`** is an optional environment variable that points to a custom root for user-level skill and command lookups.

`clawcli` does **not** support OpenAI Codex sessions, the Codex CLI, or Codex session import/export. If you need to use OpenAI models (like GPT-4.1), configure the OpenAI-compatible provider as shown above in the [OpenAI-compatible endpoint](#openai-compatible-endpoint) and [OpenRouter](#openrouter) sections.

## HTTP proxy support

`clawcli` honours the standard `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` environment variables (both upper- and lower-case spellings are accepted) when issuing outbound requests to Anthropic, OpenAI-, and xAI-compatible endpoints. Set them before launching the CLI and the underlying `reqwest` client will be configured automatically.

### Environment variables

```bash
export HTTPS_PROXY="http://proxy.corp.example:3128"
export HTTP_PROXY="http://proxy.corp.example:3128"
export NO_PROXY="localhost,127.0.0.1,.corp.example"

cd rust
./target/debug/clawcli prompt "hello via the corporate proxy"
```

### Programmatic `proxy_url` config option

As an alternative to per-scheme environment variables, the `ProxyConfig` type exposes a `proxy_url` field that acts as a single catch-all proxy for both HTTP and HTTPS traffic. When `proxy_url` is set it takes precedence over the separate `http_proxy` and `https_proxy` fields.

```rust
use api::{build_http_client_with, ProxyConfig};

// From a single unified URL (config file, CLI flag, etc.)
let config = ProxyConfig::from_proxy_url("http://proxy.corp.example:3128");
let client = build_http_client_with(&config).expect("proxy client");

// Or set the field directly alongside NO_PROXY
let config = ProxyConfig {
    proxy_url: Some("http://proxy.corp.example:3128".to_string()),
    no_proxy: Some("localhost,127.0.0.1".to_string()),
    ..ProxyConfig::default()
};
let client = build_http_client_with(&config).expect("proxy client");
```

### Notes

- When both `HTTPS_PROXY` and `HTTP_PROXY` are set, the secure proxy applies to `https://` URLs and the plain proxy applies to `http://` URLs.
- `proxy_url` is a unified alternative: when set, it applies to both `http://` and `https://` destinations, overriding the per-scheme fields.
- `NO_PROXY` accepts a comma-separated list of host suffixes (for example `.corp.example`) and IP literals.
- Empty values are treated as unset, so leaving `HTTPS_PROXY=""` in your shell will not enable a proxy.
- If a proxy URL cannot be parsed, `clawcli` falls back to a direct (no-proxy) client so existing workflows keep working; double-check the URL if you expected the request to be tunnelled.

## Common operational commands

```bash
cd rust
./target/debug/clawcli status
./target/debug/clawcli sandbox
./target/debug/clawcli agents
./target/debug/clawcli mcp
./target/debug/clawcli skills
./target/debug/clawcli system-prompt --cwd .. --date 2026-04-04
```

## Session management

REPL turns are persisted under `.claw/sessions/` in the current workspace.

```bash
cd rust
./target/debug/clawcli --resume latest
./target/debug/clawcli --resume latest /status /diff
```

Useful interactive commands include `/help`, `/status`, `/cost`, `/config`, `/session`, `/model`, `/permissions`, and `/export`.

## Config file resolution order

Runtime config is loaded in this order, with later entries overriding earlier ones:

1. `~/.claw.json`
2. `~/.config/claw/settings.json`
3. `<repo>/.claw.json`
4. `<repo>/.claw/settings.json`
5. `<repo>/.claw/settings.local.json`

## Mock parity harness

The workspace includes a deterministic Anthropic-compatible mock service and parity harness.

```bash
cd rust
./scripts/run_mock_parity_harness.sh
```

Manual mock service startup:

```bash
cd rust
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0
```

## Verification

```bash
cd rust
cargo test --workspace
```

## Workspace overview

Current Rust crates:

- `api`
- `commands`
- `compat-harness`
- `mock-anthropic-service`
- `plugins`
- `runtime`
- `rusty-claude-cli`
- `telemetry`
- `tools`
