#!/usr/bin/env bash
# Claw Code installer — Linux backend.
#
# Invoked by install/install.py. Also runnable standalone:
#   bash install/backends/linux.sh --release
#
# Same behavior as the macOS backend, with Linux-specific prerequisite hints.
# Builds `clawcli`, installs it to a bin dir, updates PATH,
# and runs a smoke test.
set -euo pipefail

# ---------------------------------------------------------------------------
# Pretty printing (mirrors install.py)
# ---------------------------------------------------------------------------

if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
    C_RESET="$(tput sgr0)"; C_BOLD="$(tput bold)"; C_DIM="$(tput dim)"
    C_RED="$(tput setaf 1)"; C_GREEN="$(tput setaf 2)"; C_YELLOW="$(tput setaf 3)"
    C_BLUE="$(tput setaf 4)"; C_CYAN="$(tput setaf 6)"
else
    C_RESET=""; C_BOLD=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_CYAN=""
fi

info()  { printf '%s  ->%s %s\n' "${C_CYAN}" "${C_RESET}" "$1"; }
ok()    { printf '%s  ok%s %s\n' "${C_GREEN}" "${C_RESET}" "$1"; }
warn()  { printf '%s  warn%s %s\n' "${C_YELLOW}" "${C_RESET}" "$1"; }
error() { printf '%s  error%s %s\n' "${C_RED}" "${C_RESET}" "$1" 1>&2; }

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------

PROFILE="debug"
SKIP_VERIFY="0"
INSTALL_DIR=""
NO_PATH_UPDATE="0"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --release)        PROFILE="release";;
        --debug)          PROFILE="debug";;
        --no-verify)      SKIP_VERIFY="1";;
        --no-path-update) NO_PATH_UPDATE="1";;
        --install-dir)
            [ "$#" -ge 2 ] || { error "--install-dir requires a value"; exit 2; }
            INSTALL_DIR="$2"; shift;;
        -h|--help)
            echo "Usage: bash install/backends/linux.sh [--release|--debug] [--no-verify] [--install-dir DIR] [--no-path-update]"
            exit 0;;
        *) error "unknown argument: $1"; exit 2;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Resolve paths
# ---------------------------------------------------------------------------

RUST_DIR="${CLAW_RUST_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/rust}"
if [ ! -f "${RUST_DIR}/Cargo.toml" ]; then
    error "Could not find rust/Cargo.toml (CLAW_RUST_DIR=${RUST_DIR})"
    exit 1
fi

if [ -z "${INSTALL_DIR}" ]; then
    if [ -n "${CARGO_HOME:-}" ]; then
        INSTALL_DIR="${CARGO_HOME}/bin"
    elif [ -d "${HOME}/.cargo/bin" ]; then
        INSTALL_DIR="${HOME}/.cargo/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
    fi
fi

TARGET_DIR="${RUST_DIR}/target/${PROFILE}"

# ---------------------------------------------------------------------------
# Linux prereq hints (non-fatal; cargo will surface real failures)
# ---------------------------------------------------------------------------

if ! command -v pkg-config >/dev/null 2>&1; then
    warn "pkg-config not found — may be required for OpenSSL-linked crates"
    info "  Debian/Ubuntu: sudo apt-get install -y pkg-config libssl-dev"
    info "  Fedora/RHEL:   sudo dnf install -y pkgconf-pkg-config openssl-devel"
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

CARGO_FLAGS=("build" "--workspace")
if [ "${PROFILE}" = "release" ]; then
    CARGO_FLAGS+=("--release")
fi

info "running: cargo ${CARGO_FLAGS[*]}"
(
    cd "${RUST_DIR}"
    CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" cargo "${CARGO_FLAGS[@]}"
)

BIN_PATH="${TARGET_DIR}/clawcli"
if [ ! -x "${BIN_PATH}" ]; then
    error "Expected binary not found: ${BIN_PATH}"
    exit 1
fi
ok "built ${BIN_PATH}"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

mkdir -p "${INSTALL_DIR}"
install -m 0755 "${TARGET_DIR}/clawcli" "${INSTALL_DIR}/clawcli"
ok "installed ${INSTALL_DIR}/clawcli"

for LEGACY_BIN in claw cliclaw; do
    if [ -e "${INSTALL_DIR}/${LEGACY_BIN}" ] || [ -L "${INSTALL_DIR}/${LEGACY_BIN}" ]; then
        rm -f "${INSTALL_DIR}/${LEGACY_BIN}"
        ok "removed legacy ${INSTALL_DIR}/${LEGACY_BIN}"
    fi
    rm -f "${TARGET_DIR}/${LEGACY_BIN}"
done

# ---------------------------------------------------------------------------
# PATH update (idempotent) — bash + zsh
# ---------------------------------------------------------------------------

path_contains_install_dir() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) return 0;;
        *) return 1;;
    esac
}

UPDATED_RC=""
if [ "${NO_PATH_UPDATE}" = "0" ]; then
    for RCFILE in "${HOME}/.bashrc" "${HOME}/.zshrc"; do
        if [ -f "${RCFILE}" ]; then
            MARKER="export PATH=\"${INSTALL_DIR}:\$PATH\""
            if ! grep -qF "${MARKER}" "${RCFILE}" 2>/dev/null; then
                printf '\n# Added by clawcli installer\n%s\n' "${MARKER}" >> "${RCFILE}"
                UPDATED_RC="${UPDATED_RC} ${RCFILE##*/}"
            fi
        fi
    done

    if ! path_contains_install_dir; then
        PATH="${INSTALL_DIR}:${PATH}"
        export PATH
    fi
fi

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------

if [ "${SKIP_VERIFY}" = "1" ]; then
    warn "verification skipped (--no-verify)"
else
    info "running: clawcli --version"
    if VERSION_OUT="$("${INSTALL_DIR}/clawcli" --version 2>&1)"; then
        ok "clawcli --version -> ${VERSION_OUT}"
    else
        error "clawcli --version failed:"; printf '%s\n' "${VERSION_OUT}" 1>&2; exit 1
    fi
fi

# ---------------------------------------------------------------------------
# Next steps
# ---------------------------------------------------------------------------

cat <<EOF

${C_GREEN}Claw Code is built and installed.${C_RESET}

  Binary:    ${C_BOLD}${INSTALL_DIR}/clawcli${C_RESET}
  Profile:   ${PROFILE}

  ${C_DIM}# clawcli — the standard CLI${C_RESET}
  clawcli prompt "summarize this repository"

EOF

if [ -n "${UPDATED_RC}" ]; then
    printf '\n%sPATH added to:%s%s\n' "${C_BOLD}" "${C_RESET}" "${UPDATED_RC}"
    printf 'Open a new terminal (or %s) so the new PATH takes effect.\n' "source ${HOME}/.bashrc"
elif [ "${NO_PATH_UPDATE}" = "1" ]; then
    printf '\nPATH was not modified (--no-path-update).\n'
else
    printf '\nInstall directory was already on PATH.\n'
fi
