#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

echo "=== Clippy ==="
cargo clippy -- -D warnings

echo "=== Coverage (includes tests) ==="
cargo llvm-cov --fail-under-lines 85 --ignore-filename-regex 'cli/src/main'
