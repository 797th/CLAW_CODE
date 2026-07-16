# Claw Code

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://img.shields.io/badge/LazyCodex-codex%20for%20no--brainers-111111?style=for-the-badge&logo=github&logoColor=white" alt="LazyCodex banner" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://img.shields.io/badge/Gajae--Code-red--claw%20agent%20harness-B22222?style=for-the-badge&logo=github&logoColor=white" alt="Gajae-Code banner" />
  </a>
</p>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://opengraph.githubassets.com/lazycodex-card/code-yeongyu/lazycodex" alt="LazyCodex GitHub card" width="280" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://opengraph.githubassets.com/gajae-code-card/Yeachan-Heo/gajae-code" alt="Gajae-Code GitHub card" width="280" />
  </a>
</p>

<h3 align="center">start with the real crab-powered harnesses</h3>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex"><b>github.com/code-yeongyu/lazycodex</b></a>
  <br/>
  <a href="https://github.com/Yeachan-Heo/gajae-code"><b>github.com/Yeachan-Heo/gajae-code</b></a>
</p>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://img.shields.io/badge/Open-LazyCodex-111111?style=flat-square&logo=github&logoColor=white" alt="Open LazyCodex on GitHub" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://img.shields.io/badge/Open-Gajae--Code-B22222?style=flat-square&logo=github&logoColor=white" alt="Open Gajae-Code on GitHub" />
  </a>
</p>

<p align="center">
  <a href="https://discord.gg/GtjhvgjnV">
    <img src="https://img.shields.io/badge/Discord-join%20the%20harness%20lab-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join the harness lab on Discord" />
  </a>
  <a href="https://discord.gg/4Rt79F7dF">
    <img src="https://img.shields.io/badge/Discord-join%20the%20crab%20tank-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join the crab tank on Discord" />
  </a>
</p>

<p align="center">
  Join the Discords:
  <a href="https://discord.gg/GtjhvgjnV"><b>ultraworkers discord</b></a>
  ·
  <a href="https://discord.gg/4Rt79F7dF"><b>gajae-code discord</b></a>
</p>

> [!IMPORTANT]
> **Claw Code is not the serious production project here.**
> This repository is closer to a museum exhibit than a product pitch, a crustacean-run artifact kept alive by clawed gajaes, swept and labeled by agents, and automatically maintained according to the harnesses above.
>
> As already described in the project philosophy, this is not meant to be hand-operated like a normal product repo. It is an **agent-managed exhibit**: the harnesses plan, execute, verify, label, and preserve the artifact while the crabs keep the tank running.
>
> If you want to actually run work, start with **[LazyCodex](https://github.com/code-yeongyu/lazycodex)** or **[Gajae-Code](https://github.com/Yeachan-Heo/gajae-code)**. If you want to inspect the strange little fossil of the Claw Code moment, continue below.
>
> For the longer public explanation behind this philosophy, see [here](https://x.com/realsigridjin/status/2039472968624185713).

<p align="center">
  <a href="https://github.com/ultraworkers/claw-code">ultraworkers/claw-code</a>
  ·
  <a href="./USAGE.md">Usage</a>
  ·
  <a href="./docs/planning/ROADMAP.md">Roadmap</a>
  ·
  <a href="./CONTRIBUTING.md">Contributing</a>
  ·
  <a href="./SECURITY.md">Security</a>
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

### Always-on Caveman responses

`clawcli` includes Caveman's default full communication style in its built-in
system prompt. It keeps technical content intact while dropping filler,
articles, and verbosity across interactive, one-shot, resumed, and
model-switched sessions, so no `/caveman` command or skill installation is
needed. Say `normal mode` or `stop caveman` to use normal prose.

`clawcli` also includes an always-on Superpowers-style development workflow.
For non-trivial changes it automatically applies focused discovery,
brainstorming, planning, TDD, systematic debugging, review, and verification.
Clear requests and small edits stay proportionate; no plugin installation or
explicit skill invocation is required.

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
