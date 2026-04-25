---
targets:
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
  - crates/db/src/**/*.rs
severity: warning
verified: 2026-04-25T13:34:59Z
---

# Flow 14: Issue List and Filtering

Create multiple issues and verify list filtering by status, assignee, parent, and blocked-on.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-14 bash /fixtures/init.sh
docker exec ns2-flow-14 bash /fixtures/start-server.sh
```

Create agent types for assignees:

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer."'
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 agent new --name "qa" --description "QA tester" --body "You are a QA tester."'
```

## Steps

### Step 1: Create several issues with different assignees

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue new --title "Build login page" --body "Implement the login page" --assignee swe | tee /tmp/issue1.txt'
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue new --title "Test login page" --body "Write tests for login" --assignee qa | tee /tmp/issue2.txt'
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue new --title "Build signup page" --body "Implement signup" --assignee swe | tee /tmp/issue3.txt'
```

Expected: three 4-character issue IDs printed, one per command.

### Step 2: List all issues

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list'
```

Expected output — all three issues in a table (includes `branch` column):
```
id      title                 status    assignee    branch    created_at
<id>    Build signup page     open      swe         <slug>    ...
<id>    Test login page       open      qa          <slug>    ...
<id>    Build login page      open      swe         <slug>    ...
```

Issues are listed newest first.

### Step 3: Filter by assignee — swe

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list --assignee swe'
```

Expected: only the two `swe` issues appear (Build login page, Build signup page).

### Step 4: Filter by assignee — qa

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list --assignee qa'
```

Expected: only `Test login page` appears.

### Step 5: Filter by status — open

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list --status open'
```

Expected: all three issues (all are open).

### Step 6: Filter by status — completed (none exist)

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list --status completed'
```

Expected: `No issues found.`

### Step 7: Edit an issue title and body

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/issue1.txt)" --title "Build login page (v2)" --body "Implement login with OAuth support"'
```

Expected: exits 0.

### Step 8: Verify the edit took effect

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list'
```

Expected: the first issue now shows `Build login page (v2)` as its title.

### Step 9: Edit assignee

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/issue2.txt)" --assignee swe'
```

Expected: exits 0.

### Step 10: Filter by assignee — qa (now empty)

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list --assignee qa'
```

Expected: `No issues found.` — the qa issue was reassigned to swe.

### Step 11: Clear assignee

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/issue3.txt)" --assignee ""'
```

Expected: exits 0.

### Step 12: List issues without assignee filter shows all

```bash
docker exec ns2-flow-14 bash -c 'cd /repo && ns2 issue list' | grep -c "open"
```

Expected: `3` — all three issues still appear in the full list.

## Acceptance Criteria

- [ ] `ns2 issue list` shows all issues in a table, newest first
- [ ] `ns2 issue list --status <status>` filters by status
- [ ] `ns2 issue list --assignee <agent>` filters by assignee
- [ ] `ns2 issue list` returns `No issues found.` when no issues match
- [ ] `ns2 issue edit --id <id> --title <title>` updates the title
- [ ] `ns2 issue edit --id <id> --body <body>` updates the body
- [ ] `ns2 issue edit --id <id> --assignee <agent>` changes the assignee
- [ ] `ns2 issue edit --id <id> --assignee ""` clears the assignee
- [ ] Edits are immediately reflected in subsequent list output