#!/bin/bash
set -euo pipefail

# Only run in remote (Claude Code Web) environments
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

echo "Setting up ns2 development environment..."

# Add required rustup components (no-op if already present)
rustup component add clippy llvm-tools-preview

# Install cargo-llvm-cov if not already installed
if ! cargo llvm-cov --version &>/dev/null 2>&1; then
  echo "Installing cargo-llvm-cov..."
  cargo install cargo-llvm-cov --locked
else
  echo "cargo-llvm-cov already installed, skipping"
fi

echo "Environment setup complete."
