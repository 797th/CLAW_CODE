# Claw Code

<p align="center">
  <a href="https://github.com/ultraworkers/claw-code">ultraworkers/claw-code</a>
  ·
  <a href="./USAGE.md">Usage</a>
  ·
  <a href="./rust/README.md">Rust workspace</a>
  ·
  <a href="./PARITY.md">Parity</a>
  ·
  <a href="./docs/planning/ROADMAP.md">Roadmap</a>
  ·
  <a href="https://discord.gg/5TUQKqFWd">UltraWorkers Discord</a>
</p>

<p align="center">
  <a href="https://star-history.com/#ultraworkers/claw-code&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=ultraworkers/claw-code&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=ultraworkers/claw-code&type=Date" />
      <img alt="Star history for ultraworkers/claw-code" src="https://api.star-history.com/svg?repos=ultraworkers/claw-code&type=Date" width="600" />
    </picture>
  </a>
</p>

<p align="center">
  <img src="assets/claw-hero.jpeg" alt="Claw Code" width="300" />
</p>

Claw Code is the public Rust implementation of the `claw` CLI agent harness.
The canonical implementation lives in [`rust/`](./rust), and the current source of truth for this repository is **ultraworkers/claw-code**.

> [!IMPORTANT]
> Start with [`USAGE.md`](./USAGE.md) for build, auth, CLI, session, and parity-harness workflows. Make `claw doctor` your first health check after building, use [`rust/README.md`](./rust/README.md) for crate-level details, read [`PARITY.md`](./PARITY.md) for the current Rust-port checkpoint, and see [`docs/container.md`](./docs/container.md) for the container-first workflow.
>
> **ACP / Zed status:** `claw-code` does not ship an ACP/Zed daemon entrypoint yet. Run `claw acp` (or `claw --acp`) for the current status instead of guessing from source layout; `claw acp serve` is currently a discoverability alias only, and real ACP support remains tracked separately in `docs/planning/ROADMAP.md`.

## Current repository shape

- **`rust/`** — canonical Rust workspace and the `claw` CLI binary
- **`install/`** — cross-OS installer (`install.py`) with per-OS backends (`backends/{macos.sh,linux.sh,windows.ps1}`)
- **`USAGE.md`** — task-oriented usage guide for the current product surface
- **`PARITY.md`** — Rust-port parity status and migration notes
- **`docs/planning/`** — long-form planning artifacts (`ROADMAP.md`, `progress.txt`, `prd.json`, `dreaming.md`)
- **`PHILOSOPHY.md`** — project intent and system-design framing
- **`src/` + `tests/`** — companion Python/reference workspace and audit helpers; not the primary runtime surface

## Quick start

> [!NOTE]
> [!WARNING]
> **`cargo install claw-code` installs the wrong thing.** The `claw-code` crate on crates.io is a deprecated stub that places `claw-code-deprecated.exe` — not `claw`. Running it only prints `"claw-code has been renamed to agent-code"`. **Do not use `cargo install claw-code`.** Either build from source (this repo) or install the upstream binary:
> ```bash
> cargo install agent-code   # upstream binary — installs 'agent.exe' (Windows) / 'agent' (Unix), NOT 'agent-code'
> ```
> This repo (`ultraworkers/claw-code`) is **build-from-source only** — follow the steps below.

### One command, every OS

The installer is a single Python entry point that works identically on macOS, Linux, and Windows. It detects your OS, hands off to the matching native backend, builds both `claw` and `cliclaw`, copies them to a bin directory, and adds that directory to your PATH.

```bash
# 1. Clone
git clone https://github.com/ultraworkers/claw-code
cd claw-code

# 2. Install (same command on macOS, Linux, and Windows)
python3 install/install.py
#    add --release for an optimized build

# 3. Set your API settings for NVIDIA NIM (OpenAI-compatible)
export OPENAI_BASE_URL="https://integrate.api.nvidia.com/v1"
export OPENAI_API_KEY="nvapi-..."
#    Default model in this fork: openai/gpt-oss-120b

# 4. Verify everything is wired correctly (open a new terminal so PATH is picked up)
claw doctor

# 5. Run a prompt
claw prompt "say hello"
```

> [!NOTE]
> **Prerequisites:** Python 3 and a Rust toolchain (`cargo` + `rustc`) must be on your PATH. If Rust is missing the installer prints the install command for your OS.
>
> **What gets installed:** both `claw` (the standard CLI) and `cliclaw` (the same binary under a name that relaxes the working-directory guard — handy for launching from `C:\` or `~`). They share one `main.rs`; only the filename changes behavior at runtime.
>
> **Windows binary names:** on Windows the binaries are `claw.exe` and `cliclaw.exe`.

<details>
<summary><strong>What the installer does under the hood</strong></summary>

```
install/
  install.py            # single cross-OS entry; parses flags, detects OS, dispatches
  backends/
    macos.sh            # macOS  — cargo build, copy both bins, update zsh/bash PATH
    linux.sh            # Linux  — same shape, plus pkg-config/OpenSSL hints
    windows.ps1         # Windows— cargo build, copy both .exe, update user PATH
```

The Python entry is a dispatcher only; each backend is independently runnable in its OS's native shell. Flags are identical everywhere:

```bash
python3 install/install.py [--release|--debug] [--no-verify] [--install-dir DIR] [--no-path-update]
```

Each backend:
1. `cargo build --workspace` (debug or release) — produces `claw` and `cliclaw`.
2. Copies both binaries into a bin directory (`$CARGO_HOME/bin`, `~/.cargo/bin`, or `~/.local/bin` on Unix; `%CARGO_HOME%\bin`, `%USERPROFILE%\.cargo\bin`, or `%USERPROFILE%\.local\bin` on Windows — override with `--install-dir`).
3. Adds that directory to your PATH idempotently (Unix: appends to `~/.zshrc` / `~/.bashrc`; Windows: sets the User `Path` env var).
4. Runs `claw --version` as a smoke test (skip with `--no-verify`).

</details>

### Manual build (no installer)

If you'd rather build by hand:

```bash
cd claw-code/rust
cargo build --workspace

# macOS/Linux
./target/debug/claw doctor
./target/debug/claw prompt "say hello"

# Windows PowerShell
.\target\debug\claw.exe doctor
.\target\debug\claw.exe prompt "say hello"
```

**Binary location:** `rust/target/debug/claw` (or `claw.exe` on Windows) after a debug build; `rust/target/release/claw` after `--release`. `cargo install --path . --force` from the `rust/` directory installs `claw` to `~/.cargo/bin`.

### Windows setup

**PowerShell is a supported Windows path.** Use whichever shell works for you. The common onboarding issues on Windows are:

1. **Install Rust first** — download from <https://rustup.rs/> and run the installer. Close and reopen your terminal when it finishes.
2. **Verify Rust is on PATH:**
   ```powershell
   cargo --version
   ```
   If this fails, reopen your terminal or run the PATH setup from the Rust installer output, then retry.
3. **Clone and install** (works in PowerShell, Git Bash, or WSL):
   ```powershell
   git clone https://github.com/ultraworkers/claw-code
   cd claw-code
   python install\install.py
   ```
4. **Run** (PowerShell — open a new terminal so PATH is picked up):
   ```powershell
   $env:OPENAI_BASE_URL = "https://integrate.api.nvidia.com/v1"
   $env:OPENAI_API_KEY = "nvapi-..."
   claw.exe prompt "say hello"
   ```

**Git Bash / WSL** are optional alternatives, not requirements. If you prefer bash-style paths (`/c/Users/you/...` instead of `C:\Users\you\...`), Git Bash (ships with Git for Windows) works well. In Git Bash, the `MINGW64` prompt is expected and normal — not a broken install.

## Locate the binary and verify

After a manual `cargo build --workspace` in `claw-code/rust/`, the binaries are built but **not** installed to your system. Use the installer (`python3 install/install.py`) to install them to your PATH, or invoke them directly:

```bash
# macOS/Linux (debug build)
./rust/target/debug/claw doctor

# Windows PowerShell (debug build)
.\rust\target\debug\claw.exe doctor
```

`claw doctor` is your first health check — it validates your API key, model access, and tool configuration.

### Add to PATH

- **Easiest:** `python3 install/install.py` — builds and installs both binaries and updates your PATH for you.
- **macOS/Linux manual:** `cargo install --path . --force` from the `rust/` directory installs `claw` to `~/.cargo/bin`.
- **Windows manual:** the installer is the recommended path on Windows; it sets the User `Path` env var correctly.

### Troubleshooting

- **"command not found: claw"** — open a new terminal so the PATH update from the installer takes effect, or re-run `python3 install/install.py`.
- **"cargo was not found"** — install Rust from <https://rustup.rs/> first; the installer needs both `cargo` and `rustc` on PATH.
- **"permission denied"** — on macOS/Linux, you may need `chmod +x rust/target/debug/claw` after a manual build (rare).
- **Debug vs. release** — if the build is slow, you're in debug mode (default). Add `--release` for faster runtime, but the build itself will take 5–10 minutes.

> [!NOTE]
> **Auth:** claw requires an **API key** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.) — Claude subscription login is not a supported auth path.

Run the workspace test suite after verifying the binary works:

```bash
cd rust
cargo test --workspace
```

> NVIDIA NIM note: this fork primarily targets GPT-OSS through the OpenAI-compatible endpoint at `https://integrate.api.nvidia.com/v1`.

### The `cliclaw` launcher

The installer builds **both** `claw` and `cliclaw`. They are the same binary (one `main.rs`); only the filename changes behavior at runtime. When launched as `cliclaw`, the CLI keeps the folder you launched from as the active workspace and allows broad working directories (such as your home folder) — useful for a global, run-from-anywhere command:

```bash
cliclaw prompt "review this repository"
```

`claw` uses the safe working-directory defaults; `cliclaw` relaxes that guard. The behavior is also overridable via the `RUSTY_CLAUDE_LAUNCHER_NAME` env var. Older `cli797` binaries map to the same permissive launcher defaults for compatibility.

## Documentation map

- [`USAGE.md`](./USAGE.md) — quick commands, auth, sessions, config, parity harness
- [`rust/README.md`](./rust/README.md) — crate map, CLI surface, features, workspace layout
- [`PARITY.md`](./PARITY.md) — parity status for the Rust port
- [`rust/MOCK_PARITY_HARNESS.md`](./rust/MOCK_PARITY_HARNESS.md) — deterministic mock-service harness details
- [`docs/planning/ROADMAP.md`](./docs/planning/ROADMAP.md) — active roadmap and open cleanup work
- [`PHILOSOPHY.md`](./PHILOSOPHY.md) — why the project exists and how it is operated

## Ecosystem

Claw Code is built in the open alongside the broader UltraWorkers toolchain:

- [clawhip](https://github.com/Yeachan-Heo/clawhip)
- [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)
- [oh-my-claudecode](https://github.com/Yeachan-Heo/oh-my-claudecode)
- [oh-my-codex](https://github.com/Yeachan-Heo/oh-my-codex)
- [UltraWorkers Discord](https://discord.gg/5TUQKqFWd)

## Ownership / affiliation disclaimer

- This repository does **not** claim ownership of the original Claude Code source material.
- This repository is **not affiliated with, endorsed by, or maintained by Anthropic**.
