---
targets:
  - crates/types/src/**/*.rs
  - crates/server/src/**/*.rs
  - crates/harness/src/**/*.rs
  - crates/cli/src/main.rs
  - ns2.toml
verified: 2026-04-25T18:57:12Z
---

# Flow 25: Session Worktree Creation

When a session is started for an issue that has a branch, the harness checks for a git worktree
at `~/.ns2/<repo>/worktrees/<branch-name>`.  If it already exists the harness uses it; if not
it creates one branching from `origin/main`.  The session's working directory is set to the
worktree path.

An `ns2.toml` file at the repository root configures the worktree base path (defaults to
`~/.ns2/<repo>/worktrees/`).

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY` so the stub client is
used.

## Fixture Setup

```bash
docker exec ns2-flow-25 bash /fixtures/init.sh
docker exec ns2-flow-25 bash /fixtures/start-server.sh
```

Ensure the repo has a remote `origin` and the remote's default branch (either `origin/main` or `origin/master`, whichever the fixture uses) so worktrees can branch from it.
The fixture container's `/repo` is already a git repo; we add a local bare clone as `origin`:

```bash
docker exec ns2-flow-25 bash -c '
  git clone --bare /repo /tmp/origin-bare
  cd /repo
  git remote add origin /tmp/origin-bare 2>/dev/null || git remote set-url origin /tmp/origin-bare
  git fetch origin
  git remote set-head origin --auto
'
```

Expected: exits 0 with no error.

Create a `swe` agent:

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer."'
```

## Steps

### Step 1: Confirm ns2.toml default worktree path is documented

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && cat ns2.toml 2>/dev/null || echo "(no ns2.toml — defaults apply)"'
```

Expected: either no file (defaults apply) or a file containing a `[worktrees]` section with a
`path` key.

### Step 2: Create an issue (auto-generates branch slug)

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue new --title "Add dashboard" --body "Build dashboard component" --assignee swe | tee /tmp/issue25.txt'
```

Expected: a 4-character issue ID.

```bash
docker exec ns2-flow-25 bash -c '
  ID=$(cat /tmp/issue25.txt)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"'"'"branch":"[^"]*"'"'"' | cut -d'"'"'"'"'"' -f4)
  echo "issue branch: $BRANCH"
  echo "$BRANCH" > /tmp/issue25-branch.txt
'
```

Expected: prints `issue branch: <id>-add-dashboard`.

### Step 3: Start the issue — harness should create a worktree

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue25.txt)"'
```

Expected: exits 0. A message on stderr like `Started session <uuid> for issue <id>`.

### Step 4: Verify the worktree was created at the expected path

```bash
docker exec ns2-flow-25 bash -c '
  BRANCH=$(cat /tmp/issue25-branch.txt)
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/$BRANCH"
  echo "checking: $WORKTREE"
  test -d "$WORKTREE" && echo "worktree exists" || { echo "FAIL: worktree missing at $WORKTREE"; exit 1; }
  test -f "$WORKTREE/.git" && echo "is a worktree (.git file present)" || { echo "FAIL: .git file missing"; exit 1; }
'
```

Expected: `worktree exists` and `is a worktree (.git file present)`.

### Step 5: Verify worktree is on the correct branch

```bash
docker exec ns2-flow-25 bash -c '
  BRANCH=$(cat /tmp/issue25-branch.txt)
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/$BRANCH"
  CURRENT=$(git -C "$WORKTREE" rev-parse --abbrev-ref HEAD)
  echo "worktree branch: $CURRENT"
  if [ "$CURRENT" = "$BRANCH" ]; then echo "OK"; else echo "FAIL: on $CURRENT, expected $BRANCH"; exit 1; fi
'
```

Expected: prints the branch name and `OK`.

### Step 6: Start a second issue on the same branch — worktree is reused

```bash
docker exec ns2-flow-25 bash -c '
  BRANCH=$(cat /tmp/issue25-branch.txt)
  cd /repo && ns2 issue new --title "Dashboard followup" --body "More dashboard work" --assignee swe --branch "$BRANCH" | tee /tmp/issue25b.txt
'
```

Expected: a 4-character issue ID.

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue25b.txt)"'
```

Expected: exits 0. No error about worktree already existing.

```bash
docker exec ns2-flow-25 bash -c '
  BRANCH=$(cat /tmp/issue25-branch.txt)
  REPO_NAME=$(basename /repo)
  WORKTREE="$HOME/.ns2/$REPO_NAME/worktrees/$BRANCH"
  # git worktree list should show exactly one entry for this path
  COUNT=$(git -C /repo worktree list | grep -F "$WORKTREE" | wc -l)
  echo "worktree entries for path: $COUNT"
  if [ "$COUNT" -eq 1 ]; then echo "OK (reused)"; else echo "FAIL: expected 1 entry, got $COUNT"; exit 1; fi
'
```

Expected: `worktree entries for path: 1` and `OK (reused)`.

### Step 7: Custom worktree path via ns2.toml

```bash
docker exec ns2-flow-25 bash -c 'cat > /repo/ns2.toml <<'"'"'EOF'"'"'
[worktrees]
path = "/tmp/custom-worktrees"
EOF'
```

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue new --title "Custom path test" --body "Should land in /tmp/custom-worktrees" --assignee swe | tee /tmp/issue25c.txt'
```

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue25c.txt)"'
```

Expected: exits 0.

```bash
docker exec ns2-flow-25 bash -c '
  ID=$(cat /tmp/issue25c.txt)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"'"'"branch":"[^"]*"'"'"' | cut -d'"'"'"'"'"' -f4)
  WORKTREE="/tmp/custom-worktrees/$BRANCH"
  echo "checking: $WORKTREE"
  test -d "$WORKTREE" && echo "OK" || { echo "FAIL: worktree missing at $WORKTREE"; exit 1; }
'
```

Expected: `OK` — worktree was created at the custom path from `ns2.toml`.

### Step 8: Wait for all issues to settle

```bash
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue25.txt)"'
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue25b.txt)"'
docker exec ns2-flow-25 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue25c.txt)"'
```

Expected: all three exit 0.

## Acceptance Criteria

- [ ] `ns2.toml` at git root is read by the server/harness; `[worktrees] path = "..."` configures the base path
- [ ] Default worktree base path is `~/.ns2/<repo-basename>/worktrees/` when `ns2.toml` is absent or has no `[worktrees]` section
- [ ] On session creation for an issue with a non-empty branch: harness checks for `<worktree-base>/<branch-name>`
- [ ] If the worktree path does not exist: harness runs `git worktree add <path> -b <branch> origin/main` (or `git worktree add <path> <branch>` if the branch already exists)
- [ ] If the worktree path already exists: harness uses it as-is without calling `git worktree add`
- [ ] The session's working directory (cwd for tool execution) is set to the worktree path
- [ ] Sessions for issues with no branch (empty string) continue to use the main repo directory as cwd (backward-compatible)
- [ ] Multiple sessions on the same branch do not error — the existing worktree is silently reused

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.