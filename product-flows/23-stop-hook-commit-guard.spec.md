---
targets:
  - crates/agents/src/**/*.rs
  - crates/harness/src/**/*.rs
  - .ns2/agents/**/*.md
  - .claude/hooks/stop-commit-guard.sh
verified: 2026-04-25T21:22:22Z
---

# Flow 23: Stop Hook — Commit Guard

A project-level Stop hook checks `git status` after every agent turn loop ends. If there are
untracked, unstaged, or uncommitted changes, the hook exits non-zero and tells the model to
commit its work before stopping.

All agents in `.ns2/agents/` have `include_project_config: true` so they inherit this hook
automatically.

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY` so the stub client is
used. What we verify is hook wiring and git-status logic — not model output.

## Fixture Setup

```bash
docker exec ns2-flow-23 bash /fixtures/init.sh
docker exec ns2-flow-23 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Verify all agents in .ns2/agents/ declare `include_project_config: true`

```bash
docker exec ns2-flow-23 bash -c 'cd /repo && for f in .ns2/agents/*.md; do echo "$f:"; grep "include_project_config" "$f" || echo "  MISSING"; done'
```

Expected: every agent file prints a line containing `include_project_config: true`. No file
prints `MISSING`.

### Step 2: Verify the project stop-hook script exists and is executable

```bash
docker exec ns2-flow-23 bash -c 'test -x /repo/.claude/hooks/stop-commit-guard.sh && echo "ok"'
```

Expected: `ok`.

### Step 3: Verify the stop hook is registered in .claude/settings.json

```bash
docker exec ns2-flow-23 bash -c 'cat /repo/.claude/settings.json | jq ".hooks.Stop"'
```

Expected: JSON array with at least one entry whose `command` contains `stop-commit-guard.sh`.

### Step 4: Clean working tree — stop hook allows stopping

```bash
docker exec ns2-flow-23 bash -c 'cd /repo && git status --short'
```

Expected: no output (clean tree).

```bash
docker exec ns2-flow-23 bash -c '/repo/.claude/hooks/stop-commit-guard.sh <<'"'"'EOF'"'"'
{"session_id": "test-session"}
EOF
echo "exit: $?"'
```

Expected: `exit: 0`. Hook exits cleanly when tree is clean.

### Step 5: Dirty working tree — stop hook blocks and prints guidance

```bash
docker exec ns2-flow-23 bash -c 'echo "dirty" > /repo/dirty-file.txt'
```

```bash
docker exec ns2-flow-23 bash -c '/repo/.claude/hooks/stop-commit-guard.sh <<'"'"'EOF'"'"'
{"session_id": "test-session"}
EOF
echo "exit: $?"'
```

Expected: `exit: 1` (or any non-zero code). Output contains a message instructing the agent to
commit changes — something like `You have uncommitted changes. Please commit your work before
stopping.` (exact wording may vary).

```bash
docker exec ns2-flow-23 bash -c 'rm /repo/dirty-file.txt'
```

### Step 6: Staged but uncommitted changes — stop hook blocks

```bash
docker exec ns2-flow-23 bash -c 'cd /repo && echo "staged" > staged-file.txt && git add staged-file.txt'
```

```bash
docker exec ns2-flow-23 bash -c '/repo/.claude/hooks/stop-commit-guard.sh <<'"'"'EOF'"'"'
{"session_id": "test-session"}
EOF
echo "exit: $?"'
```

Expected: `exit: 1`. Hook prints a message about uncommitted/staged changes.

```bash
docker exec ns2-flow-23 bash -c 'cd /repo && git rm --cached staged-file.txt && rm staged-file.txt'
```

### Step 7: Session with an agent — harness invokes stop hook

```bash
docker exec ns2-flow-23 bash -c 'echo "unsaved" > /repo/unsaved.txt'
```

```bash
docker exec ns2-flow-23 bash -c 'cd /repo && ns2 session new --agent swe --message "say hello" > /tmp/sess_stop23.txt && cat /tmp/sess_stop23.txt'
```

Expected: UUID printed.

```bash
docker exec ns2-flow-23 bash -c 'ns2 session tail --id "$(cat /tmp/sess_stop23.txt)" 2>&1 | tail -5'
```

Expected: output contains a message from the hook (`uncommitted changes` or similar) injected as
a follow-up user message, causing the agent to continue its loop. The session does NOT reach
`[done]` in the first turn.

```bash
docker exec ns2-flow-23 bash -c 'rm /repo/unsaved.txt'
```

### Step 8: After cleanup, agent eventually stops cleanly

```bash
docker exec ns2-flow-23 bash -c 'ns2 session tail --id "$(cat /tmp/sess_stop23.txt)"'
```

Expected: output eventually contains `[done]` after the working tree becomes clean (either the
stub model loop exhausts or the tree is already clean by the time the next stop fires).

## Acceptance Criteria

- [ ] `.claude/hooks/stop-commit-guard.sh` exists, is executable, reads JSON from stdin, and runs `git status --short` in `CLAUDE_PROJECT_DIR` (or the repo root)
- [ ] Hook exits 0 when `git status --short` produces no output (clean tree)
- [ ] Hook exits non-zero and prints a commit instruction message when there are any untracked, unstaged, or staged-but-uncommitted changes
- [ ] `.claude/settings.json` includes a `Stop` hook entry pointing to `stop-commit-guard.sh`
- [ ] Every file in `.ns2/agents/` has `include_project_config: true` in its frontmatter
- [ ] The harness invokes Stop hooks after `stop_reason == "end_turn"`; a non-zero exit causes the hook's stdout to be sent as a new user message and the loop continues
- [ ] A clean working tree allows the Stop hook to pass and the session to reach `completed`
- [ ] The commit guard hook works correctly when `CLAUDE_PROJECT_DIR` is set (mirrors the env var convention from Claude Code)

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.