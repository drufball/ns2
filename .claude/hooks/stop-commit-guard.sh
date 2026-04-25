#!/usr/bin/env bash
# Stop hook: blocks agent completion when there are uncommitted changes.
# Reads a JSON payload from stdin (e.g. {"session_id": "..."}).
# Exits 0 if working tree is clean; exits 1 with instructions if dirty.

INPUT=$(cat)  # consume stdin (not currently used, but required by hook protocol)

REPO_ROOT="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null)}"

if [ -z "$REPO_ROOT" ]; then
    echo "stop-commit-guard: could not determine repo root" >&2
    exit 0  # fail open if we can't determine root
fi

STATUS=$(git -C "$REPO_ROOT" status --short 2>/dev/null)

if [ -n "$STATUS" ]; then
    echo "You have uncommitted changes. Please commit your work before stopping."
    echo ""
    echo "Uncommitted changes:"
    echo "$STATUS"
    exit 1
fi

exit 0
