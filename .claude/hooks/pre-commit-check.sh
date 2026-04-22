#!/usr/bin/env bash
# Reads the PreToolUse hook JSON from stdin.
# Blocks git add / git commit if the verification script fails.

INPUT=$(cat)
COMMAND=$(printf '%s' "$INPUT" | jq -r '.tool_input.command // ""')

# Only intercept git add or git commit commands
if ! printf '%s' "$COMMAND" | grep -qE 'git (add|commit)'; then
    exit 0
fi

OUTPUT=$("$CLAUDE_PROJECT_DIR/scripts/verify.sh" 2>&1)
STATUS=$?

if [ "$STATUS" -ne 0 ]; then
    printf 'Pre-commit verification failed. Fix before committing:\n\n%s\n' "$OUTPUT" >&2
    exit 2
fi
