#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root/rust"

echo "=== baseline ==="
cargo run --quiet -p runtime --example caveman_fidelity -- --mode baseline

echo
echo "=== caveman ==="
cargo run --quiet -p runtime --example caveman_fidelity -- --mode caveman
