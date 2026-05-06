#!/usr/bin/env bash
# Stop hook: blocks the session from completing if there are uncommitted changes
# or commits that have not been pushed to the remote.
#
# Works for both Claude Code sessions (where CLAUDE_PROJECT_DIR is set) and
# ns2 harness sessions (where the hook subprocess cwd is the session worktree).

GIT_DIR="${CLAUDE_PROJECT_DIR:-$(pwd)}"

# 1. Check for uncommitted / untracked changes.
STATUS=$(git -C "$GIT_DIR" status --short 2>/dev/null)
if [ -n "$STATUS" ]; then
  echo "You have uncommitted changes. Please commit all your work before stopping." >&2
  exit 2
fi

# 2. Check for commits that exist locally but have not been pushed to any remote.
HAS_REMOTE=$(git -C "$GIT_DIR" remote 2>/dev/null)
if [ -n "$HAS_REMOTE" ]; then
  UNPUSHED=$(git -C "$GIT_DIR" log HEAD --oneline --not --remotes 2>/dev/null | wc -l | tr -d ' ')
  if [ "$UNPUSHED" -gt 0 ]; then
    echo "You have ${UNPUSHED} unpushed commit(s). Please push your work before stopping." >&2
    exit 2
  fi
fi

exit 0
