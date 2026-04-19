#!/usr/bin/env bash
# source this script — do not run it directly

# BASH_SOURCE[0] is empty in zsh (1-indexed arrays); use $0 there instead
if [[ -n "${ZSH_VERSION:-}" ]]; then
    WORKTREE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
else
    WORKTREE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
TEST_REPO="/tmp/ns2-test-repo"

# 1. Load .env (sets ANTHROPIC_API_KEY)
if [[ -f "$WORKTREE_DIR/.env" ]]; then
    set -a
    source "$WORKTREE_DIR/.env"
    set +a
    echo "Loaded .env"
else
    echo "Warning: no .env found at $WORKTREE_DIR/.env — ANTHROPIC_API_KEY will not be set"
fi

# 2. Create dummy git repo (idempotent)
if [[ -d "$TEST_REPO" ]]; then
    echo "Test repo already exists at $TEST_REPO"
else
    echo "Creating test repo at $TEST_REPO..."
    mkdir -p "$TEST_REPO"
    git -C "$TEST_REPO" init
    git -C "$TEST_REPO" config user.email "test@example.com"
    git -C "$TEST_REPO" config user.name "ns2 tester"
    echo "# ns2-test-repo" > "$TEST_REPO/README.md"
    git -C "$TEST_REPO" add README.md
    git -C "$TEST_REPO" commit -m "initial commit"
    echo "Test repo created."
fi

# 3. Build binary (cargo skips unchanged crates, so this is fast on re-runs)
echo "Building ns2 binary..."
if ! cargo build --manifest-path "$WORKTREE_DIR/Cargo.toml" 2>&1; then
    echo "ERROR: build failed — fix errors above before continuing"
    return 1
fi

# 4. Export NS2
export NS2="$BINARY"

echo ""
echo "============================================================"
echo "  ns2 manual testing setup complete"
echo "============================================================"
echo ""
echo "Binary:   $NS2"
echo "Data dir: ~/.ns2/ns2-test-repo/"
echo ""
echo "IMPORTANT: run ns2 commands from the test repo so git root"
echo "detection resolves to 'ns2-test-repo':"
echo ""
echo "  cd $TEST_REPO"
echo "  \$NS2 session list"
echo ""
echo "To use \$NS2 in your current shell, source this script:"
echo "  source product-flows/setup.sh"
echo "============================================================"
