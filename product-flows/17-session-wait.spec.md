---
targets:
  - crates/cli/src/main.rs
verified: 2026-04-24T15:24:42Z
---

# Flow 17: Session Wait

Block until one or more sessions reach a terminal state (`completed`, `failed`, or `cancelled`),
then print each session's final status.

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY`, which causes the stub
client to be used — sessions with an initial message will complete immediately with a canned
response.

## Fixture Setup

```bash
docker exec ns2-flow-17 bash /fixtures/init.sh
docker exec ns2-flow-17 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Create a session that will complete via the stub client

```bash
docker exec ns2-flow-17 bash -c 'cd /repo && ns2 session new --message "say hello" > /tmp/sess_a.txt && cat /tmp/sess_a.txt'
```

Expected: a UUID printed to stdout. The stub client processes the message and the session reaches
`completed` within a few seconds.

### Step 2: Wait briefly for the session to finish, then confirm status is `completed`

```bash
docker exec ns2-flow-17 bash -c 'sleep 2 && cd /repo && ns2 session list --id "$(cat /tmp/sess_a.txt)" | grep completed'
```

Expected: a table row containing `completed`.

### Step 3: `ns2 session wait` on an already-completed session returns immediately

```bash
docker exec ns2-flow-17 bash -c 'cd /repo && ns2 session wait --id "$(cat /tmp/sess_a.txt)"; echo "Exit: $?"'
```

Expected output:
```
<uuid>  completed
Exit: 0
```

One line is printed per session (`<uuid>  <status>`). Exit code is 0 because no session failed.

### Step 4: Create two more sessions and wait on all three simultaneously

```bash
docker exec ns2-flow-17 bash -c '
  cd /repo
  ns2 session new --message "say hello again" > /tmp/sess_b.txt
  ns2 session new --message "say goodbye" > /tmp/sess_c.txt
  A=$(cat /tmp/sess_a.txt)
  B=$(cat /tmp/sess_b.txt)
  C=$(cat /tmp/sess_c.txt)
  ns2 session wait --id "$A" --id "$B" --id "$C"
  echo "Exit: $?"
'
```

Expected: three lines of output (one per session), each containing a UUID and `completed`. Exit
code 0.

### Step 5: Wait on a failed session — exits non-zero

```bash
docker exec ns2-flow-17 bash -c '
  # Patch the session status to failed via the admin API
  A=$(cat /tmp/sess_a.txt)
  curl -sf -X PATCH "http://localhost:9876/sessions/$A/status" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"failed\"}" > /dev/null
  cd /repo && ns2 session wait --id "$A"
  echo "Exit: $?"
'
```

Expected output:
```
<uuid>  failed
Exit: 1
```

Exit code is 1 because a session reached `failed`.

### Step 6: Wait on a cancelled session — exits 0

```bash
docker exec ns2-flow-17 bash -c '
  B=$(cat /tmp/sess_b.txt)
  curl -sf -X PATCH "http://localhost:9876/sessions/$B/status" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"cancelled\"}" > /dev/null
  cd /repo && ns2 session wait --id "$B"
  echo "Exit: $?"
'
```

Expected output:
```
<uuid>  cancelled
Exit: 0
```

Exit code is 0 — `cancelled` is a non-failure terminal state.

### Step 7: Mix of completed and failed sessions — exits non-zero

```bash
docker exec ns2-flow-17 bash -c '
  A=$(cat /tmp/sess_a.txt)
  C=$(cat /tmp/sess_c.txt)
  cd /repo && ns2 session wait --id "$A" --id "$C"
  echo "Exit: $?"
'
```

Expected: two lines (one per session) with statuses, and `Exit: 1` because at least one failed.

### Step 8: Error — wait on a non-existent session ID

```bash
docker exec ns2-flow-17 bash -c 'cd /repo && ns2 session wait --id "00000000-0000-0000-0000-000000000000"; echo "Exit: $?"'
```

Expected: error message on stderr (e.g., `Error: session not found`) and `Exit: 1`.

### Step 9: Error — `session wait` with no `--id` flags

```bash
docker exec ns2-flow-17 bash -c 'cd /repo && ns2 session wait; echo "Exit: $?"'
```

Expected: error message on stderr (e.g., `Error: at least one --id is required`) and `Exit: 1`.

## Acceptance Criteria

- [ ] `ns2 session wait --id <uuid>` blocks until that session is in `completed`, `failed`, or `cancelled`
- [ ] On completion, prints one line per session: `<uuid>  <status>`
- [ ] Exits 0 when all sessions reached `completed` or `cancelled`
- [ ] Exits 1 when any session reached `failed`
- [ ] Accepts multiple `--id` flags and waits for all of them
- [ ] Returns immediately if all sessions are already in a terminal state
- [ ] Exits 1 with an error message on stderr if any session ID does not exist
- [ ] Exits 1 with an error message on stderr when called with no `--id` arguments
- [ ] `PATCH /sessions/:id/status` endpoint accepts `{"status":"<value>"}` and updates the session status in the DB

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.