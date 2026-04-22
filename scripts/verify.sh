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

echo "=== Spec sync ==="
cargo build -p cli -q
if ! ./ns2 spec sync; then
    echo ""
    echo "One or more spec files have changed since they were last verified."
    echo ""
    echo "To resolve:"
    echo "  1. Review the spec file(s) and file(s) listed above"
    echo "  2. Make sure the spec reflects the current implementation"
    echo "     (update the spec if code changed, or update code if spec is authoritative)"
    echo "  3. Once they agree, mark the spec verified:"
    echo "       ./ns2 spec verify <path/to/spec.spec.md>"
    exit 1
fi
