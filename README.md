# Claw Code

<p align="center">
  <a href="https://github.com/ultraworkers/claw-code">ultraworkers/claw-code</a>
  ·
  <a href="./USAGE.md">Usage</a>
  ·
  <a href="./docs/planning/ROADMAP.md">Roadmap</a>
  ·
  <a href="https://discord.gg/5TUQKqFWd">UltraWorkers Discord</a>
</p>

<p align="center">
  <img src="assets/claw-hero.jpeg" alt="Claw Code" width="300" />
</p>

**Claw Code** is a command-line AI coding assistant (the `clawcli` CLI). It's written in Rust.

You need one tool to build it: **Rust**. Get it from <https://rustup.rs/>.

---

## Install

In the project folder, run:

```bash
python3 install/install.py
```

That's it. The installer works the same on **macOS, Linux, and Windows**. It builds `clawcli` and puts it on your PATH.

> **Need Rust first?** Install it from <https://rustup.rs/>, then open a new terminal and run the install command above.

<details>
<summary>Windows? Click here.</summary>

Use PowerShell:

```powershell
python install\install.py
```

The binary is called `clawcli.exe`. The installer adds it to your PATH automatically.

</details>

---

## Start it

After install, **open a new terminal** (so your PATH updates), then run:

```bash
clawcli
```

That opens the interactive assistant. To send one prompt and exit:

```bash
clawcli prompt "say hello"
```

### Always-on concise responses

`clawcli` includes a Caveman-style concise response policy in its built-in
system prompt. It starts on the first turn for interactive, one-shot, and
resumed sessions, so no `/caveman` command or skill installation is needed.
Technical content stays intact: code, commands, paths, identifiers, URLs, and
exact errors remain unchanged. Ask for normal mode or more detail whenever a
long-form explanation is useful.

<details>
<summary>First run: connect an API</summary>

If no credentials are detected, `clawcli` opens a setup prompt. Choose an
OpenAI-compatible or Anthropic-compatible connection, enter its base URL and
API key, then enter the model name. The OpenAI-compatible option is selected
by default and starts with `https://api.openai.com/v1`, so custom gateways,
local servers, and hosted endpoints use the same flow.

The API key is entered without echo, saved to `~/.claw/.env` (or
`$CLAW_CONFIG_HOME/.env`) with user-only permissions, and the selected model is
saved to `settings.json`. To configure it non-interactively, set the matching
environment variables yourself:

```bash
# Any OpenAI-compatible service; the URL is optional for api.openai.com.
export OPENAI_BASE_URL="https://your-gateway.example/v1"
export OPENAI_API_KEY="..."
```

Or an Anthropic-compatible service:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

(Windows PowerShell: use `$env:OPENAI_API_KEY = "..."` instead of `export`.)

Then check that everything is wired up:

```bash
clawcli doctor
```

</details>

---

## What the installer does

1. Builds `clawcli` (and `cliclaw`, a copy that lets you run from any folder) from the Rust source in `rust/`.
2. Copies it into a bin folder on your PATH (`~/.cargo/bin` on mac/linux, the equivalent on Windows).
3. Runs `clawcli --version` to confirm it works.

<details>
<summary>Advanced options</summary>

```bash
python3 install/install.py --release        # optimized build (slower to compile)
python3 install/install.py --no-verify      # skip the version check
python3 install/install.py --install-dir X  # install to a specific folder
python3 install/install.py --no-path-update # don't change your PATH
```

Under the hood, `install.py` is a dispatcher that calls a per-OS backend in `install/backends/` (`macos.sh`, `linux.sh`, `windows.ps1`). Each is also runnable on its own.

</details>

---

## `clawcli` vs `cliclaw`

The installer builds both. They're the **same program** — the only difference is the name:

- **`clawcli`** — the normal command. Use this.
- **`cliclaw`** — identical, but lets you run it from any folder (even your home directory) without it complaining. Handy if you want a global "run from anywhere" command.

---

## Troubleshooting

- **`command not found: clawcli`** — open a new terminal so the PATH change takes effect.
- **`cargo was not found`** — install Rust from <https://rustup.rs/> first.
- **Build is slow** — that's normal for a first build (a few minutes). Add `--release` only if you want faster runtime.

---

## Learn more

- [`USAGE.md`](./USAGE.md) — full command reference, auth, sessions, config
- [`rust/README.md`](./rust/README.md) — Rust workspace and crate details
- [`PARITY.md`](./PARITY.md) — port status
- [`docs/planning/ROADMAP.md`](./docs/planning/ROADMAP.md) — roadmap
- [`THIRD_PARTY_NOTICES.md`](./THIRD_PARTY_NOTICES.md) — embedded dependency notices

## Ownership / affiliation disclaimer

- This repository does **not** claim ownership of the original Claude Code source material.
- This repository is **not affiliated with, endorsed by, or maintained by Anthropic**.
