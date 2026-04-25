---
targets:
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-25T18:44:20Z
---

# Flow 16: Issue Error Cases

Test error handling for invalid issue operations.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-16 bash /fixtures/init.sh
docker exec ns2-flow-16 bash /fixtures/start-server.sh
```

Create an agent type:

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer."'
```

## Steps

### Step 1: Create a test issue without assignee

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue new --title "Unassigned task" --body "No agent assigned" | tee /tmp/no_assignee.txt'
```

Expected: a 4-character issue ID.

### Step 2: Attempt to start an issue without assignee

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/no_assignee.txt)"; echo "Exit code: $?"'
```

Expected: error message indicating an assignee is required, and `Exit code: 1`.

```
Error: issue <id> has no assignee — set one with `ns2 issue edit --id <id> --assignee <agent>`
Exit code: 1
```

### Step 3: Create an issue with assignee

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue new --title "Assigned task" --body "Has an agent" --assignee swe | tee /tmp/assigned.txt'
```

Expected: a 4-character issue ID.

### Step 4: Mark issue completed directly (skip start)

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue complete --id "$(cat /tmp/assigned.txt)" --comment "Done without running"'
```

Expected: exits 0. Issues can be completed without going through `start` (useful for manual closure).

### Step 5: Attempt to start an already-completed issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/assigned.txt)"; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue <id> is already completed
Exit code: 1
```

### Step 6: Attempt to complete an already-completed issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue complete --id "$(cat /tmp/assigned.txt)" --comment "Trying again"; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue <id> is already in terminal state (completed)
Exit code: 1
```

### Step 7: Attempt to reference nonexistent issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue start --id "zzzz"; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue not found: zzzz
Exit code: 1
```

### Step 8: Attempt to create issue with missing required field

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue new --title "No body"; echo "Exit code: $?"'
```

Expected: error message about missing `--body` flag, non-zero exit code.

### Step 9: Attempt to complete without comment

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue new --title "Test" --body "Test body" --assignee swe | tee /tmp/test.txt'
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue complete --id "$(cat /tmp/test.txt)"; echo "Exit code: $?"'
```

Expected: error message about missing `--comment` flag, non-zero exit code.

### Step 10: Wait on nonexistent issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue wait --id zzzz; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue not found: zzzz
Exit code: 1
```

### Step 11: Edit nonexistent issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue edit --id zzzz --title "New title"; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue not found: zzzz
Exit code: 1
```

### Step 12: Comment on nonexistent issue

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue comment --id zzzz --body "Hello"; echo "Exit code: $?"'
```

Expected: error message and non-zero exit code.

```
Error: issue not found: zzzz
Exit code: 1
```

### Step 13: Assign to nonexistent agent type

```bash
docker exec ns2-flow-16 bash -c 'cd /repo && ns2 issue new --title "Bad agent" --body "Agent does not exist" --assignee nonexistent; echo "Exit code: $?"'
```

Expected: error message about agent not found, non-zero exit code.

```
Error: agent type 'nonexistent' not found in .ns2/agents/
Exit code: 1
```

## Acceptance Criteria

- [ ] `ns2 issue start` fails with clear error when issue has no assignee
- [ ] `ns2 issue start` fails when issue is already in terminal state (completed/failed)
- [ ] `ns2 issue complete` fails when issue is already in terminal state
- [ ] `ns2 issue complete` requires `--comment` flag
- [ ] `ns2 issue new` requires both `--title` and `--body` flags
- [ ] Operations on nonexistent issue IDs return `Error: issue not found: <id>`
- [ ] `ns2 issue new --assignee <agent>` fails if agent type doesn't exist
- [ ] All error cases exit with non-zero status code
- [ ] Error messages are actionable (tell user how to fix the problem)