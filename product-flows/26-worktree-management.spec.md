---
targets:
  - crates/cli/src/main.rs
  - crates/server/src/**/*.rs
verified: 2026-04-26T16:59:54Z
---

# Flow 26: Worktree Management Commands

`ns2 worktree` exposes three subcommands for inspecting and cleaning up git worktrees managed by
ns2: `list`, `create`, and `delete`.

- **list**   — prints all worktrees ns2 knows about for the current repo
- **create** — creates a worktree for a named branch (idempotent if it already exists)
- **delete** — removes a worktree; errors if the branch is not merged to `main` unless `--force`
  is given

Worktrees are NOT auto-deleted at session completion.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-26 bash /fixtures/init.sh
docker exec ns2-flow-26 bash /fixtures/start-server.sh
```

Set up a local `origin` remote so `git worktree add … origin/main` works:

```bash
docker exec ns2-flow-26 bash -c '
  git clone --bare /repo /tmp/origin-bare26
  cd /repo
  git remote add origin /tmp/origin-bare26 2>/dev/null || git remote set-url origin /tmp/origin-bare26
  git fetch origin
  git remote set-head origin --auto
'
```

Expected: exits 0.

## Steps

### Step 1: List worktrees — none exist yet

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree list'
```

Expected: `No worktrees found.` (or a table with only the main worktree header and zero data
rows). Exit code 0.

### Step 2: Create a worktree for a new branch

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree create --branch "feature/wt-alpha"'
```

Expected: exits 0. Output on stderr such as `Created worktree for branch feature/wt-alpha at
<path>`.

### Step 3: Verify the worktree exists on disk

```bash
docker exec ns2-flow-26 bash -c '
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/feature/wt-alpha"
  test -d "$WORKTREE" && echo "exists" || { echo "FAIL"; exit 1; }
  test -f "$WORKTREE/.git" && echo "is worktree" || { echo "FAIL: not a worktree"; exit 1; }
'
```

Expected: `exists` and `is worktree`.

### Step 4: Create a second worktree

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree create --branch "feature/wt-beta"'
```

Expected: exits 0.

### Step 5: List worktrees — shows both

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree list'
```

Expected: a table with at least two data rows, one for `feature/wt-alpha` and one for
`feature/wt-beta`, showing their path and branch name.

### Step 6: Create an already-existing worktree (idempotent)

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree create --branch "feature/wt-alpha"; echo "exit: $?"'
```

Expected: `exit: 0`. The command does not error when the worktree already exists.

### Step 7: Delete a worktree whose branch has no unique commits (is merged)

Since our test branches were created from `origin/main` and have no extra commits they are
trivially "merged":

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree delete --branch "feature/wt-beta"'
```

Expected: exits 0. Output such as `Deleted worktree for branch feature/wt-beta`.

### Step 8: Verify the deleted worktree is gone from disk

```bash
docker exec ns2-flow-26 bash -c '
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/feature/wt-beta"
  test -d "$WORKTREE" && { echo "FAIL: still exists"; exit 1; } || echo "gone"
'
```

Expected: `gone`.

### Step 9: List worktrees — only wt-alpha remains

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree list'
```

Expected: only `feature/wt-alpha` appears in the table. `feature/wt-beta` is absent.

### Step 10: Add a commit to wt-alpha so it is unmerged

```bash
docker exec ns2-flow-26 bash -c '
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/feature/wt-alpha"
  echo "unmerged change" > "$WORKTREE/unmerged.txt"
  git -C "$WORKTREE" add unmerged.txt
  git -C "$WORKTREE" commit -m "unmerged commit"
'
```

Expected: exits 0. A commit is added to the branch.

### Step 11: Delete an unmerged worktree without --force — should error

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree delete --branch "feature/wt-alpha"; echo "exit: $?"'
```

Expected: a non-zero exit code and an error message such as:
```
Error: branch feature/wt-alpha has unmerged commits. Use --force to delete anyway.
exit: 1
```

### Step 12: Delete with --force overrides the unmerged check

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree delete --branch "feature/wt-alpha" --force'
```

Expected: exits 0.

### Step 13: Verify wt-alpha is gone

```bash
docker exec ns2-flow-26 bash -c '
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/feature/wt-alpha"
  test -d "$WORKTREE" && { echo "FAIL: still exists"; exit 1; } || echo "gone"
'
```

Expected: `gone`.

### Step 14: Delete a nonexistent worktree — should error

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 worktree delete --branch "feature/no-such"; echo "exit: $?"'
```

Expected: non-zero exit code and error message:
```
Error: no worktree found for branch feature/no-such
exit: 1
```

### Step 15: Session completion does NOT auto-delete worktrees

Create an issue, start it, wait for completion, then verify the worktree is still present:

```bash
docker exec ns2-flow-26 bash -c 'cd /repo && ns2 agent new --name "swe" --description "swe" --body "You are a swe." 2>/dev/null || true'
```

```bash
docker exec ns2-flow-26 bash -c '
  cd /repo
  git remote add origin /tmp/origin-bare26 2>/dev/null || true
  git fetch origin 2>/dev/null || true
  git remote set-head origin --auto 2>/dev/null || true
  ID=$(ns2 issue new --title "Worktree persist test" --body "Should not delete worktree on completion" --assignee swe)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"branch":"[^"]*"' | cut -d'"' -f4)
  echo "$BRANCH" > /tmp/persist-branch.txt
  ns2 issue start --id "$ID"
  ns2 issue wait --id "$ID"
  echo "issue done"
'
```

Expected: `issue done`.

```bash
docker exec ns2-flow-26 bash -c '
  BRANCH=$(cat /tmp/persist-branch.txt)
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/$BRANCH"
  test -d "$WORKTREE" && echo "worktree still present (OK)" || { echo "FAIL: worktree was auto-deleted"; exit 1; }
'
```

Expected: `worktree still present (OK)`.

## Acceptance Criteria

- [ ] `ns2 worktree list` prints a table of all ns2-managed worktrees for the current repo; prints `No worktrees found.` when there are none
- [ ] `ns2 worktree create --branch <branch>` creates a worktree at the configured path; exits 0 if it already exists (idempotent)
- [ ] `ns2 worktree delete --branch <branch>` removes the worktree and its directory; runs `git worktree remove` and then `git branch -d`
- [ ] `ns2 worktree delete` errors with a clear message and exit code 1 when the branch has commits not reachable from `main`
- [ ] `ns2 worktree delete --force` skips the merged check and deletes unconditionally
- [ ] `ns2 worktree delete` errors with a clear message when no worktree exists for the given branch
- [ ] Worktrees are NOT deleted when a session completes or fails
- [ ] `ns2 worktree list` output includes at least `branch` and `path` columns

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.