#!/usr/bin/env python3
"""Claw Code cross-OS installer.

Single entry point that works identically on macOS, Linux, and Windows:

    python3 install.py                 # debug build (default)
    python3 install.py --release       # optimized release build
    python3 install.py --no-verify     # skip post-install smoke test
    python3 install.py --install-dir /opt/clawcli/bin
    python3 install.py --no-path-update
    python3 install.py --help

This script is a dispatcher only. It parses a common set of flags, detects the
host platform, and hands off to the matching native backend under
``install/backends/``:

    macOS / Linux  ->  bash backends/<os>.sh <flags>
    Windows        ->  powershell backends/windows.ps1 <flags>

The backends own the real build/copy/PATH logic so each one stays in its OS's
idiomatic shell and is independently runnable and debuggable. Both ``clawcli`` and
``cliclaw`` are built and installed on every OS (same binary, two names — the
``cliclaw`` name relaxes the working-directory guard at runtime).
"""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
RUST_DIR = REPO_ROOT / "rust"
BACKENDS_DIR = SCRIPT_DIR / "backends"

# ANSI colors — disabled when stdout is not a TTY or NO_COLOR is set.
_USE_COLOR = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None


def _c(code: str) -> str:
    return code if _USE_COLOR else ""


RESET = _c("\033[0m")
BOLD = _c("\033[1m")
DIM = _c("\033[2m")
RED = _c("\033[31m")
GREEN = _c("\033[32m")
YELLOW = _c("\033[33m")
BLUE = _c("\033[34m")
CYAN = _c("\033[36m")

# Steps that print identically across all backends (backends mirror this order).
TOTAL_STEPS = 6
_CURRENT_STEP = 0


# ---------------------------------------------------------------------------
# Pretty printing
# ---------------------------------------------------------------------------

def banner() -> None:
    art = r"""
   ____  _                   ____          _
  / ___|| |  __ _ __      __ / ___|___   __| | ___
 | |    | | / _` |\ \ /\ / /| |   / _ \ / _` |/ _ \
 | |___ | || (_| | \ V  V / | |__| (_) | (_| |  __/
  \____||_| \__,_|  \_/\_/   \____\___/ \__,_|\___|
"""
    print(f"{BOLD}{art}{RESET}")
    print(f"{DIM}Claw Code installer{RESET}")


def step(name: str) -> None:
    global _CURRENT_STEP
    _CURRENT_STEP += 1
    print(
        f"\n{BLUE}[{_CURRENT_STEP}/{TOTAL_STEPS}]{RESET} {BOLD}{name}{RESET}"
    )


def info(msg: str) -> None:
    print(f"{CYAN}  ->{RESET} {msg}")


def ok(msg: str) -> None:
    print(f"{GREEN}  ok{RESET} {msg}")


def warn(msg: str) -> None:
    print(f"{YELLOW}  warn{RESET} {msg}")


def error(msg: str) -> None:
    print(f"{RED}  error{RESET} {msg}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------

USAGE = """\
Usage: python3 install.py [options]

Options:
  --release             Build the optimized release profile (slower, smaller).
  --debug               Build the debug profile (default, faster compile).
  --no-verify           Skip the post-install verification step.
  --install-dir <path>  Override the destination bin directory.
  --no-path-update      Do not modify the user PATH.
  -h, --help            Show this help text and exit.

Environment overrides:
  CLAW_BUILD_PROFILE    debug | release
  CLAW_SKIP_VERIFY      set to 1 to skip verification
"""


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

class Options:
    def __init__(self) -> None:
        profile = os.environ.get("CLAW_BUILD_PROFILE", "debug")
        if profile not in ("debug", "release"):
            sys.exit(f"{RED}error:{RESET} invalid CLAW_BUILD_PROFILE: {profile}")
        self.profile: str = profile
        self.skip_verify: bool = os.environ.get("CLAW_SKIP_VERIFY") == "1"
        self.install_dir: str | None = None
        self.no_path_update: bool = False

    def to_backend_argv(self) -> list[str]:
        """Flags passed through to the native backend, normalized."""
        args: list[str] = []
        args.append("--release" if self.profile == "release" else "--debug")
        if self.skip_verify:
            args.append("--no-verify")
        if self.no_path_update:
            args.append("--no-path-update")
        if self.install_dir:
            args.extend(["--install-dir", self.install_dir])
        return args


def parse_args(argv: list[str]) -> Options:
    opts = Options()
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg in ("-h", "--help"):
            print(USAGE)
            sys.exit(0)
        elif arg == "--release":
            opts.profile = "release"
        elif arg == "--debug":
            opts.profile = "debug"
        elif arg == "--no-verify":
            opts.skip_verify = True
        elif arg == "--no-path-update":
            opts.no_path_update = True
        elif arg == "--install-dir":
            if i + 1 >= len(argv):
                sys.exit(f"{RED}error:{RESET} --install-dir requires a value")
            opts.install_dir = argv[i + 1]
            i += 1
        else:
            sys.exit(f"{RED}error:{RESET} unknown argument: {arg}\n\n{USAGE}")
        i += 1
    return opts


# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

def detect_platform() -> str:
    """Return one of: 'macos', 'linux', 'windows'."""
    system = platform.system().lower()
    if system == "darwin":
        return "macos"
    if system == "linux":
        return "linux"
    if system in ("windows", "cygwin") or sys.platform in ("win32", "cygwin"):
        return "windows"
    sys.exit(
        f"{RED}error:{RESET} unsupported or unknown platform: {system}\n"
        f"Supported: macOS, Linux, Windows."
    )


# ---------------------------------------------------------------------------
# Prerequisite checks (run before dispatching)
# ---------------------------------------------------------------------------

def check_prereqs(os_family: str) -> None:
    cargo = shutil.which("cargo")
    rustc = shutil.which("rustc")
    if not cargo or not rustc:
        error("Rust toolchain not found on PATH (need both 'cargo' and 'rustc').")
        info("Install Rust via rustup:")
        if os_family == "windows":
            info("  Download and run https://win.rustup.rs/x86_64")
            info("  then open a new terminal and retry.")
        else:
            info("  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh")
            info('  source "$HOME/.cargo/env"')
        sys.exit(1)

    cargo_ver = subprocess.run(
        ["cargo", "--version"], capture_output=True, text=True
    ).stdout.strip()
    rustc_ver = subprocess.run(
        ["rustc", "--version"], capture_output=True, text=True
    ).stdout.strip()
    ok(f"cargo found: {cargo_ver}")
    ok(f"rustc found: {rustc_ver}")

    if shutil.which("git"):
        git_ver = subprocess.run(
            ["git", "--version"], capture_output=True, text=True
        ).stdout.strip()
        ok(f"git found:  {git_ver}")
    else:
        warn("git not found — some workflows (login, session export) may degrade")


# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------

def dispatch(os_family: str, opts: Options) -> int:
    backend_argv = opts.to_backend_argv()
    info("flags: " + " ".join(backend_argv))

    if os_family in ("macos", "linux"):
        backend = BACKENDS_DIR / f"{os_family}.sh"
        # Pass the rust dir + profile context via env so the backend can stay simple,
        # but flags are the source of truth on argv.
        env = os.environ.copy()
        env["CLAW_REPO_ROOT"] = str(REPO_ROOT)
        env["CLAW_RUST_DIR"] = str(RUST_DIR)
        cmd = ["bash", str(backend), *backend_argv]
    else:  # windows
        backend = BACKENDS_DIR / "windows.ps1"
        env = os.environ.copy()
        env["CLAW_REPO_ROOT"] = str(REPO_ROOT)
        env["CLAW_RUST_DIR"] = str(RUST_DIR)
        cmd = [
            "powershell",
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", str(backend),
            *backend_argv,
        ]

    if not backend.exists():
        sys.exit(f"{RED}error:{RESET} backend missing: {backend}")

    try:
        return subprocess.call(cmd, env=env)
    except FileNotFoundError as exc:
        if os_family == "windows":
            error("PowerShell was not found. This installer needs powershell.exe.")
        else:
            error("bash was not found.")
        sys.exit(f"{exc}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main(argv: list[str]) -> int:
    opts = parse_args(argv)

    banner()
    step("Detecting host environment")
    os_family = detect_platform()
    ok(f"supported platform detected: {os_family}")

    step("Locating the Rust workspace")
    if not (RUST_DIR / "Cargo.toml").exists():
        error(f"Could not find rust/Cargo.toml at {RUST_DIR}")
        error("Repository layout looks unexpected.")
        return 1
    ok(f"workspace at {RUST_DIR}")

    step("Checking prerequisites")
    check_prereqs(os_family)

    # Steps 4-6 (build, install, verify/next-steps) run inside the native
    # backend — the backend owns the real build/copy/PATH/verify logic.
    info(
        f"handing off to {os_family} backend — building both clawcli + cliclaw "
        f"({opts.profile}); first build may take a few minutes"
    )

    return dispatch(os_family, opts)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
