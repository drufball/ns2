#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

echo "=== Clippy ==="
cargo clippy -- -D warnings

echo "=== Coverage (includes tests) ==="
# Build --ignore-filename-regex flags from the [[file_ignore]] table in
# crates/arch-tests/coverage-ignores.toml, so that file is the single source
# of truth for what's excluded.
IGNORE_FLAGS=$(python3 - <<'EOF'
import sys
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        sys.exit(0)  # no TOML parser available; run without ignores

with open("crates/arch-tests/coverage-ignores.toml", "rb") as f:
    data = tomllib.load(f)

flags = []
for entry in data.get("file_ignore", []):
    flags.append("--ignore-filename-regex")
    flags.append(entry["path"])
print(" ".join(flags))
EOF
)

# shellcheck disable=SC2086
cargo llvm-cov --fail-under-lines 85 $IGNORE_FLAGS
