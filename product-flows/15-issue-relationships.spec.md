---
targets:
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-25T11:26:14Z
---

# Flow 15: Issue Relationships

Test parent-child relationships and blocked-on dependencies between issues.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-15 bash /fixtures/init.sh
docker exec ns2-flow-15 bash /fixtures/start-server.sh
```

Create an agent type:

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer."'
```

## Steps

### Step 1: Create a parent issue

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Epic: User Authentication" --body "Implement complete user auth system" --assignee swe | tee /tmp/parent.txt'
```

Expected: a 4-character issue ID.

### Step 2: Create child issues with parent

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Login endpoint" --body "POST /login" --assignee swe --parent "$(cat /tmp/parent.txt)" | tee /tmp/child1.txt'
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Logout endpoint" --body "POST /logout" --assignee swe --parent "$(cat /tmp/parent.txt)" | tee /tmp/child2.txt'
```

Expected: two 4-character issue IDs.

### Step 3: Filter by parent

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --parent "$(cat /tmp/parent.txt)"'
```

Expected: only the two child issues appear (Login endpoint, Logout endpoint).

### Step 4: Create blocking issues

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Database schema" --body "Create users table" --assignee swe | tee /tmp/blocker1.txt'
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Session storage" --body "Redis session store" --assignee swe | tee /tmp/blocker2.txt'
```

Expected: two 4-character issue IDs.

### Step 5: Create an issue blocked by multiple others

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue new --title "Implement auth middleware" --body "JWT validation middleware" --assignee swe --blocked-on "$(cat /tmp/blocker1.txt)" --blocked-on "$(cat /tmp/blocker2.txt)" | tee /tmp/blocked.txt'
```

Expected: a 4-character issue ID.

### Step 6: Filter by blocked-on

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --blocked-on "$(cat /tmp/blocker1.txt)"'
```

Expected: only `Implement auth middleware` appears — it's blocked by blocker1.

### Step 7: Edit to add a parent to an existing issue

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/blocker1.txt)" --parent "$(cat /tmp/parent.txt)"'
```

Expected: exits 0.

### Step 8: Filter by parent shows updated list

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --parent "$(cat /tmp/parent.txt)"'
```

Expected: now shows three issues — Login endpoint, Logout endpoint, and Database schema.

### Step 9: Edit to clear parent

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/blocker1.txt)" --parent ""'
```

Expected: exits 0.

### Step 10: Filter by parent shows original list

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --parent "$(cat /tmp/parent.txt)"'
```

Expected: back to two issues — Login endpoint, Logout endpoint.

### Step 11: Edit to replace blocked-on list

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/blocked.txt)" --blocked-on "$(cat /tmp/blocker1.txt)"'
```

Expected: exits 0. The blocked-on list is now only blocker1 (blocker2 removed).

### Step 12: Filter by blocked-on blocker2 (now empty)

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --blocked-on "$(cat /tmp/blocker2.txt)"'
```

Expected: `No issues found.`

### Step 13: Edit to clear blocked-on list entirely

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue edit --id "$(cat /tmp/blocked.txt)" --blocked-on'
```

Expected: exits 0. Passing `--blocked-on` with no values clears the list.

### Step 14: Filter by blocked-on blocker1 (now empty)

```bash
docker exec ns2-flow-15 bash -c 'cd /repo && ns2 issue list --blocked-on "$(cat /tmp/blocker1.txt)"'
```

Expected: `No issues found.`

## Acceptance Criteria

- [ ] `ns2 issue new --parent <id>` creates an issue with a parent link
- [ ] `ns2 issue new --blocked-on <id1> --blocked-on <id2>` creates an issue blocked by multiple issues
- [ ] `ns2 issue list --parent <id>` filters to show only children of that parent
- [ ] `ns2 issue list --blocked-on <id>` filters to show only issues blocked by that issue
- [ ] `ns2 issue edit --id <id> --parent <pid>` sets or changes the parent
- [ ] `ns2 issue edit --id <id> --parent ""` clears the parent
- [ ] `ns2 issue edit --id <id> --blocked-on <id1>` replaces the blocked-on list
- [ ] `ns2 issue edit --id <id> --blocked-on` (no values) clears the blocked-on list
- [ ] Relationship changes are immediately reflected in list filters