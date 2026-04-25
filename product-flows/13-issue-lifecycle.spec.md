---
targets:
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-25T10:03:20Z
---

# Flow 13: Issue Lifecycle

Create an issue, assign it to an agent, start a session, wait for completion, and mark it done.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

An agent type must exist for the assignee.

## Fixture Setup

```bash
docker exec ns2-flow-13 bash /fixtures/init.sh
docker exec ns2-flow-13 bash /fixtures/start-server.sh
```

Create an agent type for the test:

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion."'
```

## Steps

### Step 1: Create an issue with an assignee

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe | tee /tmp/issue_id.txt'
```

Expected: a 4-character issue ID printed to stdout (e.g., `a1b2`).

### Step 2: Verify the issue exists with status open

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue list --status open'
```

Expected output — a table showing the issue:
```
id      title                 status    assignee    created_at
a1b2    Add a greeting        open      swe         2026-04-24 12:00:00 UTC
```

The issue ID matches what was printed in Step 1, status is `open`, and assignee is `swe`.

### Step 3: Start the issue (creates and runs a session)

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue_id.txt)"'
```

Expected output on stderr:
```
Started session <uuid> for issue <id>
```

The command creates a session using the `swe` agent, sends the issue title and body as the opening message, and links the session to the issue.

### Step 4: Verify issue status is running

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue list --status running'
```

Expected: the issue now shows with status `running`.

### Step 5: Wait for the issue to complete

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue_id.txt)"'
```

Expected: the command blocks until the session completes, then exits 0. No output on success.

### Step 6: Verify issue status is completed

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue list --status completed'
```

Expected: the issue shows with status `completed`.

### Step 7: Verify the agent posted a final-turn comment automatically

```bash
docker exec ns2-flow-13 bash -c '
  ISSUE=$(cat /tmp/issue_id.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
agent_comments = [c for c in comments if c[\"author\"] == \"swe\"]
print(\"Agent comments:\", len(agent_comments))
if len(agent_comments) >= 1:
    print(\"OK\")
else:
    print(\"FAIL\")
    sys.exit(1)
"
'
```

Expected: `OK` — the `issue_watcher` task automatically posted the agent's final turn text
as a comment (author = `"swe"`) before transitioning the issue to `completed`.

### Step 8: Add a manual completion comment

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue complete --id "$(cat /tmp/issue_id.txt)" --comment "Verified: hello.txt created with correct content."'
```

Expected output on stderr:
```
Issue <id> marked completed.
```

### Step 9: Add a regular comment

```bash
docker exec ns2-flow-13 bash -c 'cd /repo && ns2 issue comment --id "$(cat /tmp/issue_id.txt)" --body "Good work!" --author reviewer'
```

Expected: command exits 0 with no error.

## Acceptance Criteria

- [ ] `ns2 issue new` prints a 4-character issue ID to stdout
- [ ] `ns2 issue new --assignee <agent>` stores the assignee
- [ ] New issues start in `open` status
- [ ] `ns2 issue start --id <id>` creates a session linked to the issue
- [ ] `ns2 issue start` sets the issue status to `running`
- [ ] The session uses the issue's assignee agent type
- [ ] The session receives the issue title and body as the opening message
- [ ] `ns2 issue wait --id <id>` blocks until the issue reaches a terminal state
- [ ] `ns2 issue wait` exits 0 when the issue completes successfully
- [ ] When the session completes, the agent's final turn text is automatically posted as a comment with `author == assignee`
- [ ] `ns2 issue complete --id <id> --comment "..."` adds a manual summary comment (in addition to the auto-posted agent comment)
- [ ] `ns2 issue comment` adds comments with the specified author
- [ ] Issue status transitions: open → running → completed