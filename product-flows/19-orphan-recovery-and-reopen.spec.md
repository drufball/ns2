---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
  - crates/cli/src/**/*.rs
verified: 2026-04-25T20:27:55Z
---

# Flow 19: Server Restart Orphan Recovery and Issue Reopen

Verify that issues whose linked session was `running` at the time of a server restart are
automatically recovered to `failed` with a system comment, and that `ns2 issue reopen`
moves a `failed` issue back to `open` so work can resume.

No API key is required — sessions are created with an initial message but the stub client
is used (no `ANTHROPIC_API_KEY` in the environment).

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY`; the stub client
is used — sessions with an initial message will reach `completed` quickly. To reliably
test orphan recovery we freeze a session in `running` state using the admin PATCH endpoint
before restarting.

## Fixture Setup

```bash
docker exec ns2-flow-19 bash /fixtures/init.sh
docker exec ns2-flow-19 bash /fixtures/start-server.sh
```

Create an agent type:

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer."'
```

## Steps

### Step 1: Create an issue and start it

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue new --title "Orphan test" --body "This issue will be orphaned" --assignee swe | tee /tmp/issue.txt'
```

Expected: a 4-character issue ID.

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue.txt)"'
```

Expected: `Started session <uuid> for issue <id>` on stderr. Issue is now `running`.

### Step 2: Allow the stub session to complete, then freeze issue back to `running`

```bash
docker exec ns2-flow-19 bash -c 'sleep 2 && cd /repo && ns2 issue list --id "$(cat /tmp/issue.txt)"'
```

Expected: issue shows `completed` (stub client ran). Now we manually force it back to
`running` with its session ID to simulate a mid-flight restart:

```bash
docker exec ns2-flow-19 bash -c '
  ISSUE=$(cat /tmp/issue.txt)
  # Look up the session ID from the issue record
  SESS=$(curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d[\"session_id\"])")
  echo "$SESS" > /tmp/sess.txt
  # Force the issue back to running so orphan recovery has something to find
  curl -sf -X PATCH "http://localhost:9876/issues/$ISSUE/status" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"running\"}" > /dev/null
  # Force the session back to running
  curl -sf -X PATCH "http://localhost:9876/sessions/$SESS/status" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"running\"}" > /dev/null
  echo "Forced to running"
'
```

Expected: `Forced to running`.

### Step 3: Restart the server

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 server stop && sleep 1 && ns2 server start && sleep 1'
```

Expected: server stops and restarts cleanly.

### Step 4: Verify orphaned session is now `failed`

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 session list --id "$(cat /tmp/sess.txt)" | grep failed'
```

Expected: a table row containing `failed`. The orphan sweep ran on startup and marked the
`running` session `failed`.

### Step 5: Verify orphaned issue is now `failed`

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue list --id "$(cat /tmp/issue.txt)" | grep failed'
```

Expected: a table row containing `failed`. The orphan sweep also marked the linked issue
`failed`.

### Step 6: Verify the system comment was posted on the issue

```bash
docker exec ns2-flow-19 bash -c '
  ISSUE=$(cat /tmp/issue.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
found = any(\"session lost on server restart\" in c[\"body\"] for c in comments)
print(\"FOUND\" if found else \"NOT FOUND\")
print(\"Comments:\", json.dumps(comments, indent=2))
"
'
```

Expected: `FOUND` — at least one comment with body containing `session lost on server restart`.

### Step 7: Reopen the failed issue

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue reopen --id "$(cat /tmp/issue.txt)"'
```

Expected: exits 0 with output on stderr:
```
Issue <id> reopened.
```

### Step 8: Verify issue is now `open`

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue list --id "$(cat /tmp/issue.txt)" | grep open'
```

Expected: a table row containing `open`. The `session_id` link has been cleared.

### Step 9: Verify existing comments are preserved after reopen

```bash
docker exec ns2-flow-19 bash -c '
  ISSUE=$(cat /tmp/issue.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
found = any(\"session lost on server restart\" in c[\"body\"] for c in comments)
print(\"PRESERVED\" if found else \"LOST\")
"
'
```

Expected: `PRESERVED` — the system comment added during orphan recovery is still present.

### Step 10: Error — reopen a non-failed issue

```bash
docker exec ns2-flow-19 bash -c '
  cd /repo
  ID=$(ns2 issue new --title "Open issue" --body "Still open" --assignee swe)
  ns2 issue reopen --id "$ID"
  echo "Exit: $?"
'
```

Expected: error message on stderr (e.g., `Error: issue <id> is not in failed state`) and
non-zero exit code.

### Step 11: Error — reopen a nonexistent issue

```bash
docker exec ns2-flow-19 bash -c 'cd /repo && ns2 issue reopen --id "zzzz"; echo "Exit: $?"'
```

Expected: `Error: issue not found: zzzz` on stderr and non-zero exit code.

### Step 12: Verify non-`running` sessions are not affected by orphan sweep

```bash
docker exec ns2-flow-19 bash -c '
  cd /repo
  # Create a completed session
  SESS=$(ns2 session new --message "hello")
  sleep 2
  # Restart the server
  ns2 server stop && sleep 1 && ns2 server start && sleep 1
  # The completed session should remain completed
  ns2 session list --id "$SESS" | grep -v running | grep -v failed
  echo "Exit: $?"
'
```

Expected: the session row shows `completed` (or `created` if the stub did not run), not
`failed`. Only `running` sessions are swept.

## Acceptance Criteria

- [ ] On server restart, sessions with status `running` are transitioned to `failed`
- [ ] Issues linked to swept sessions are transitioned to `failed`
- [ ] A system comment `"session lost on server restart"` is posted on each affected issue
- [ ] Sessions in `completed`, `created`, `cancelled`, or `failed` status are not affected
      by the orphan sweep
- [ ] `ns2 issue reopen --id <id>` transitions a `failed` issue to `open`
- [ ] `ns2 issue reopen` prints `Issue <id> reopened.` on stderr and exits 0
- [ ] `ns2 issue reopen` clears the `session_id` link on the issue
- [ ] `ns2 issue reopen` preserves all existing comments
- [ ] `ns2 issue reopen` fails with a clear error if the issue is not in `failed` state
- [ ] `ns2 issue reopen` fails with `Error: issue not found: <id>` for unknown IDs
- [ ] `POST /issues/:id/reopen` endpoint (or equivalent) is exposed for CLI use
- [ ] `PATCH /issues/:id/status` endpoint accepts status values for test control

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.