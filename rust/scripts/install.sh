#!/bin/bash
set -e

# Build the release binary
cargo build --release

# Link to ~/.local/bin
mkdir -p "$HOME/.local/bin"
ln -sf "$(pwd)/target/release/clawcli" "$HOME/.local/bin/clawcli"
rm -f "$HOME/.local/bin/claw" "$HOME/.local/bin/cliclaw"

echo "✓ Claw installed to ~/.local/bin/clawcli"
