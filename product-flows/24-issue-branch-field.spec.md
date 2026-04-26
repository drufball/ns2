---
targets:
  - crates/types/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
verified: 2026-04-26T17:28:06Z
---

# Flow 24: Issue Branch Field

Issues gain a `branch` field that is automatically derived from the issue id + title when no
parent exists, inherited from the parent issue when one is provided, and overridden by an
explicit `--branch` flag.  No worktrees are created in this flow — it only verifies that the
field is stored, displayed, and correctly defaulted.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-24 bash /fixtures/init.sh
docker exec ns2-flow-24 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Create an issue with no parent and no explicit branch

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue new --title "Add login page" --body "Implement the login page" | tee /tmp/issue_a.txt'
```

Expected: a 4-character issue ID printed to stdout.

### Step 2: Inspect the branch auto-assigned to the issue

```bash
docker exec ns2-flow-24 bash -c '
  ID=$(cat /tmp/issue_a.txt)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"'"'"branch":"[^"]*"'"'"' | cut -d'"'"'"'"'"' -f4)
  echo "branch: $BRANCH"
  echo "$BRANCH" | grep -E "^[a-z0-9]+-[a-z0-9-]+$" && echo "OK" || { echo "FAIL: bad slug $BRANCH"; exit 1; }
'
```

Expected: prints `branch: <id>-add-login-page` (the id slug joined with the slugified title),
followed by `OK`.

### Step 3: Create a parent issue

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue new --title "Epic: Auth System" --body "Full auth" | tee /tmp/parent.txt'
```

Expected: a 4-character issue ID.

### Step 4: Create a child issue — branch should inherit from parent

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue new --title "Login endpoint" --body "POST /login" --parent "$(cat /tmp/parent.txt)" | tee /tmp/child.txt'
```

Expected: a 4-character issue ID.

### Step 5: Verify child inherits parent branch

```bash
docker exec ns2-flow-24 bash -c '
  PARENT=$(cat /tmp/parent.txt)
  CHILD=$(cat /tmp/child.txt)
  PARENT_BRANCH=$(curl -sf "http://localhost:9876/issues/$PARENT" | grep -o '"branch":"[^"]*"' | cut -d'"' -f4)
  CHILD_BRANCH=$(curl -sf "http://localhost:9876/issues/$CHILD" | grep -o '"branch":"[^"]*"' | cut -d'"' -f4)
  echo "parent branch: $PARENT_BRANCH"
  echo "child branch:  $CHILD_BRANCH"
  if [ "$PARENT_BRANCH" = "$CHILD_BRANCH" ]; then echo "OK"; else echo "FAIL: branches differ"; exit 1; fi
'
```

Expected: both branches are identical and `OK` is printed.

### Step 6: Create an issue with an explicit --branch flag

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue new --title "Hotfix login bug" --body "Fix null pointer" --branch "hotfix/login-null" | tee /tmp/explicit.txt'
```

Expected: a 4-character issue ID.

### Step 7: Verify the explicit branch was stored as-is

```bash
docker exec ns2-flow-24 bash -c '
  ID=$(cat /tmp/explicit.txt)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"'"'"branch":"[^"]*"'"'"' | cut -d'"'"'"'"'"' -f4)
  echo "branch: $BRANCH"
  if [ "$BRANCH" = "hotfix/login-null" ]; then echo "OK"; else echo "FAIL: got $BRANCH"; exit 1; fi
'
```

Expected: `branch: hotfix/login-null` and `OK`.

### Step 8: Verify branch appears in issue list output

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue list'
```

Expected: a table is printed that includes a `branch` column. All three issues appear with their respective branch values.

### Step 9: Edit an issue to change its branch

```bash
docker exec ns2-flow-24 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/issue_a.txt)" --branch "feature/new-branch"'
```

Expected: exits 0.

### Step 10: Verify the edited branch is reflected

```bash
docker exec ns2-flow-24 bash -c '
  ID=$(cat /tmp/issue_a.txt)
  BRANCH=$(curl -sf "http://localhost:9876/issues/$ID" | grep -o '"'"'"branch":"[^"]*"'"'"' | cut -d'"'"'"'"'"' -f4)
  if [ "$BRANCH" = "feature/new-branch" ]; then echo "OK"; else echo "FAIL: got $BRANCH"; exit 1; fi
'
```

Expected: `OK`.

## Acceptance Criteria

- [ ] `Issue` type gains a `branch: String` field (non-optional)
- [ ] DB schema is migrated to add a `branch` column (non-null, default = empty string for existing rows)
- [ ] `ns2 issue new` with no `--parent` and no `--branch`: branch = `<id>-<slugified-title>` (lowercase, spaces and special chars replaced with `-`, consecutive dashes collapsed)
- [ ] `ns2 issue new --parent <pid>`: branch inherits parent's branch value verbatim
- [ ] `ns2 issue new --branch <val>`: branch is stored exactly as provided (overrides both auto and parent-inherit)
- [ ] `ns2 issue edit --id <id> --branch <val>` updates the branch
- [ ] `ns2 issue list` table includes a `branch` column
- [ ] `GET /issues/<id>` JSON response includes `"branch": "..."`

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.