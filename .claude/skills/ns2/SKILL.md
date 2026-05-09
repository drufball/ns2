---
name: ns2
description: Orchestrate work using ns2 — start issues, monitor progress token-efficiently, navigate work branches, and manage multi-issue workflows. Use whenever you're about to create ns2 issues or manage ongoing agent sessions.
---

# ns2 orchestration skill

## Session start protocol

Always do this at the start of every coding session, because prior PRs may have landed:

```bash
ns2 server stop 2>/dev/null || true
cargo build --release 2>&1 | tail -3
ns2 server start
sleep 2
ns2 issue list
```

**Why:** The server runs from whatever binary and branch are checked out. Stale servers silently run old code.

## Starting issues

Work in ns2 is managed by creating issues. Every issue is given a dedicated agent session to complete the issue based on `--assignee`.

```bash
# Create, start, and wait in one command
id=$(ns2 issue new --title "..." --body "..." --assignee <agent> --status in_progress --wait)
```

Keep the alternative 2-step pattern for cases where you need the ID before waiting:

```bash
# Two-step: get ID first, then start
id=$(ns2 issue new --title "..." --body "..." --assignee <agent>)
ns2 issue set-status --id "$id" --status in_progress
```

## Token-efficient monitoring

**Never** run `ns2 session tail` without `--turns` — it will stream all previously completed turns and consume your context. If you want to monitor `ns2` as it works, use `tail` with `timeout` to review it's progress for a short period and then detach, preserving your context window.

### Quick progress check
```bash
# Peek at last 2 turns, exit after <timeout> seconds (session continues in background)
ns2 session tail --id "$SESSION" --turns 2 --timeout <timeout>
```

### Wait for completion
```bash
ns2 issue wait --id "$id" --timeout <timeout>
```

## Branch navigation

ns2 completes issues on git worktrees. When a new issue with no parent is spawned, it has a new branch + worktree created for it. By default, child issues will share the same branch and worktree to minimise coordination overhead. If you want to set a specific branch for an issue instead of using the default generated one, you can use the `--branch` flag.

**CRITICAL:** These worktrees are separate git checkouts on different branches. When you're in the main repo working on the orchestration layer, you are NOT in the issue's worktree. Keep track of which directory you're in.

All agents are instructed to commit & push their work when they finish. During normal operation, NEVER try to navigate to the working directory of worktree and inspect changes there. Simply `git pull` the branch to your cwd and review changes there.

## Debugging ns2

### Checking work locally

ns2 is alpha software, so you may have to debug its work occasionally. ONLY WHEN NEEDED FOR DEBUGGING: ns2 creates worktrees under `~/.ns2/<repo-name>/worktrees/<branch>/`. You can nagivate there to investigate and fix issues.


### Reopening Failed Issues

If an issue fails (e.g. rate limit cascade), reopen with context before restarting:

```bash
ns2 issue reopen --id "$id" --comment "Restarting after rate limit. Previous approach was correct, pick up from where you left off."
ns2 issue set-status --id "$id" --status in_progress
```

## Sending Corrections Mid-Session

If you see the agent going wrong during a tail check:

```bash
ns2 session send --id "$SESSION" --message "Instruction to fix the issue..."
```
