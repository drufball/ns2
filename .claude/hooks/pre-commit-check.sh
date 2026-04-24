#!/usr/bin/env bash
# Reads the PreToolUse hook JSON from stdin.
# Blocks git add / git commit if the verification script fails.

INPUT=$(cat)
COMMAND=$(printf '%s' "$INPUT" | jq -r '.tool_input.command // ""')

# Pre-commit: run full verification before git add / git commit
if printf '%s' "$COMMAND" | grep -qE 'git (add|commit)'; then
    OUTPUT=$("$CLAUDE_PROJECT_DIR/scripts/verify.sh" 2>&1)
    STATUS=$?
    if [ "$STATUS" -ne 0 ]; then
        printf 'Pre-commit verification failed. Fix before committing:\n\n%s\n' "$OUTPUT" >&2
        exit 2
    fi
    exit 0
fi

# Pre-PR: run spec sync --error-on-warnings before gh pr create
if printf '%s' "$COMMAND" | grep -q 'gh pr create'; then
    OUTPUT=$(cd "$CLAUDE_PROJECT_DIR" && cargo build -p cli -q 2>&1 && ns2 spec sync --error-on-warnings 2>&1)
    STATUS=$?
    if [ "$STATUS" -ne 0 ]; then
        printf 'Pre-PR spec check failed. Verify stale specs before opening a PR:\n\n%s\n' "$OUTPUT" >&2
        exit 2
    fi
    exit 0
fi
