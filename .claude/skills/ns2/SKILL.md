---
name: ns2
description: Orchestrate work using ns2 — start issues, monitor progress token-efficiently, navigate worktrees, and manage multi-issue workflows. Use whenever you're about to create ns2 issues or manage ongoing agent sessions.
---

# ns2 Orchestration Skill

## Session Start Protocol

Always do this at the start of every coding session, because prior PRs may have landed:

```bash
ns2 server stop 2>/dev/null || true
cargo build --release 2>&1 | tail -3
ns2 server start
sleep 2
ns2 issue list
```

**Why:** The server runs from whatever binary and branch are checked out. Stale servers silently run old code.

## Issue Lifecycle

```bash
# Create and immediately start
id=$(ns2 issue new --title "Short title" --body "Full context..." --assignee swe)
SESSION=$(ns2 issue start --id "$id" 2>&1 | awk '/Session:/{print $NF}')
echo "Issue: $id  Session: $SESSION"
```

Capture the session ID at start time — you need it later for tailing. `ns2 issue start` prints `Started issue <id>. Session: <uuid>` to **stderr**, so redirect with `2>&1` before piping.

## Token-Efficient Monitoring

**Never** run `ns2 session tail` without `--turns` — it will stream indefinitely and consume your context.

### Quick progress check (do this every few minutes)
```bash
# Peek at last 2 turns, kill after 10 seconds (macOS: timeout not in PATH, use background+kill)
ns2 session tail --id "$SESSION" --turns 2 &
TAIL_PID=$!
sleep 10
kill $TAIL_PID 2>/dev/null
wait $TAIL_PID 2>/dev/null
```

### Wait for completion in the background
```bash
# Start the wait in background, check status when done
ns2 issue wait --id "$id" &
WAIT_PID=$!

# ... do other work or check other issues ...

wait $WAIT_PID
echo "Exit: $?  (0=completed, non-zero=failed)"
```

### Monitoring multiple issues in parallel
Spawn one background Agent per issue. Each agent:
1. Captures the session ID when starting the issue
2. Runs `ns2 issue wait --id "$id" &` in the background
3. Every few turns checks `timeout 10 ns2 session tail --id "$SESSION" --turns 2`
4. Reports back when the issue completes or fails

This keeps each issue's monitoring contained without flooding the main context.

## Worktree Navigation

ns2 creates worktrees under `~/.ns2/<repo-name>/worktrees/<branch>/`. For this repo:

```
~/.ns2/ns2/worktrees/<issue-id>-<slug>/
```

**CRITICAL:** These worktrees are separate git checkouts on different branches. When you're in the main repo working on the orchestration layer, you are NOT in the issue's worktree. Keep track of which directory you're in.

### Reading an issue's code without switching branches
```bash
# List what an issue has changed
git -C ~/.ns2/ns2/worktrees/<branch>/ log --oneline -5
git -C ~/.ns2/ns2/worktrees/<branch>/ diff origin/main --stat

# Read a specific file from an issue's worktree
cat ~/.ns2/ns2/worktrees/<branch>/crates/harness/src/lib.rs
```

### Never do this accidentally
```bash
# WRONG: this cd + git operation could confuse you about which branch you're on
cd ~/.ns2/ns2/worktrees/<branch>
git add . && git commit  # wrong branch!

# RIGHT: use -C to stay grounded
git -C ~/.ns2/ns2/worktrees/<branch>/ log --oneline -3
```

### Finding an issue's branch name
```bash
ns2 issue list | grep <issue-id>
# Branch column shows the branch name = the worktree dir name
```

## Rebasing After a Merge

When you fix a blocking issue and merge it to main, you need to rebase your current branch and restart the server:

```bash
git fetch origin
git rebase origin/main
cargo build --release 2>&1 | tail -3
ns2 server stop 2>/dev/null || true
ns2 server start
sleep 2
```

This ensures the server runs the merged code so subsequent worktrees (created by new issues) branch off the merged state.

## Parallel Issue Orchestration

For multiple independent issues, start them all, then monitor:

```bash
# Start all issues
id1=$(ns2 issue new --title "Fix A" --body "..." --assignee swe)
s1=$(ns2 issue start --id "$id1" 2>&1 | grep Session: | awk '{print $NF}')

id2=$(ns2 issue new --title "Fix B" --body "..." --assignee swe)
s2=$(ns2 issue start --id "$id2" 2>&1 | grep Session: | awk '{print $NF}')

# Wait for all
ns2 issue wait --id "$id1" --id "$id2"
```

For sequential dependencies (issue B needs issue A's merge first):
1. Start A, wait for merge
2. `git fetch origin && git rebase origin/main` to pick up the merge
3. `ns2 server stop && ns2 server start` to restart with new binary
4. Start B (its worktree will branch from the updated main)

## Checking Progress Without Over-Tailing

If you need to check many issues at once, look at status first, then tail only failing ones:

```bash
ns2 issue list --status running
ns2 issue list --status failed

# Only tail a specific failing session (background+kill for macOS)
ns2 session tail --id "$SESSION" --turns 3 & sleep 10; kill %1 2>/dev/null; wait %1 2>/dev/null
```

## Reopening Failed Issues

If an issue fails (e.g. rate limit cascade), reopen with context before restarting:

```bash
ns2 issue reopen --id "$id" --comment "Restarting after rate limit. Previous approach was correct, pick up from where you left off." --start
```

## Completing Issues

After an agent's PR is merged or work is done:

```bash
ns2 issue complete --id "$id" --comment "PR merged. Summary: <what was done>"
```

## Sending Corrections Mid-Session

If you see the agent going wrong during a tail check:

```bash
ns2 session send --id "$SESSION" --message "Stop — you're modifying the wrong file. The fix belongs in crates/harness/src/lib.rs, not workspace. Start over on that approach."
```

## The Stop Hook Will Commit Your Uncommitted Changes

The ns2 stop hook runs when a session ends. If it finds uncommitted changes in the MAIN working directory (your branch, not the agent worktree), it will commit them automatically. This is by design — it prevents the agent from stopping with dirty state.

**Implication:** If you have staged-but-uncommitted work on your branch when a swe agent's session ends, the stop hook will commit it with a generated message. This is usually fine, but be aware that the hook might commit partial work if you haven't finished staging everything.

## Spec Sync Blocks Commits on All Stale Specs

`ns2 spec sync` (which runs before every commit) fails if ANY error-severity spec is stale — even specs you didn't touch. Pre-existing stale specs from prior PRs will block your commits.

**Workarounds:**
- Verify specs that you've reviewed and confirmed are still accurate: `ns2 spec verify <path>`
- For specs known to be drifted (e.g. agent-harness.spec.md per GH #22), lower their severity to `warning` in the frontmatter — this marks them as known-aspirational without falsely claiming they're verified
- This is a known workflow friction point that needs rethinking at the project level

## Common Help Text Gaps (ns2 UX notes)

Things the current help text doesn't make obvious:
- `ns2 issue start` prints the session UUID to **stderr** — capture with `2>&1 | awk '/Session:/{print $NF}'`
- `session tail --turns 0` skips all history and shows only new events (useful for watching without replaying)
- `ns2 worktree list` shows the full path for each worktree — use this to find where an issue's code lives
- `ns2 issue wait` exits immediately if all issues are already in terminal state — safe to call unconditionally
- The server must be rebuilt (`cargo build --release`) when code changes; just restarting an old binary picks up no changes
- `timeout` is not available on macOS by default — use background + kill pattern instead
