#!/usr/bin/env bash
# Stop the server and remove all test state. Safe to run even if nothing is running.

if [[ -n "${ZSH_VERSION:-}" ]]; then
    WORKTREE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
else
    WORKTREE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

NS2_BIN="$WORKTREE_DIR/target/debug/ns2"
DATA_DIR="$HOME/.ns2/ns2-test-repo"
PID_FILE="$DATA_DIR/server-9876.pid"

# Stop via CLI if binary exists
if [[ -x "$NS2_BIN" ]]; then
    "$NS2_BIN" server stop 2>/dev/null || true
fi

# Kill by PID as fallback (handles cases where CLI stop fails)
if [[ -f "$PID_FILE" ]]; then
    pid=$(cat "$PID_FILE" 2>/dev/null)
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
fi

# Brief pause for port to clear
sleep 0.5

rm -rf /tmp/ns2-test-repo
rm -rf "$DATA_DIR"

echo "Cleanup complete"
