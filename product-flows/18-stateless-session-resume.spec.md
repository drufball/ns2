---
targets:
  - crates/server/src/**/*.rs
  - crates/harness/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-25T10:03:20Z
---

# Flow 18: Stateless Session Resume

Verify that a `completed` session can accept new messages, that the harness reconstructs
full conversation context from the DB (no in-memory dependency), and that the session
returns to `completed` after the follow-up turn.

This flow exercises the stateless-session guarantee: if a session's harness has exited,
`send_message` spawns a fresh harness that loads all turn history from SQLite so the
agent has full context — no in-memory conversation state is required.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the
container.

## Fixture Setup

```bash
docker exec ns2-flow-18 bash /fixtures/init.sh
docker exec ns2-flow-18 bash /fixtures/start-server.sh
docker exec ns2-flow-18 bash /fixtures/seeded-files.sh
```

## Steps

### Step 1: Start a session with an initial message

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session new --message "My secret number is 4219. Acknowledge it." > /tmp/sess.txt && cat /tmp/sess.txt'
```

Expected: a UUID printed to stdout.

### Step 2: Tail until the session completes

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/sess.txt)"'
```

Expected: stream ends with `[done]`. The agent's response acknowledges the number `4219`.

### Step 3: Confirm the session is `completed`

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session list --id "$(cat /tmp/sess.txt)" | grep completed'
```

Expected: a table row containing `completed`.

### Step 4: Restart the server (simulate process restart)

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 server stop && sleep 1 && ns2 server start && sleep 1'
```

Expected: server stops and restarts cleanly. The session remains `completed` in the DB.

### Step 5: Confirm session is still `completed` after restart

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session list --id "$(cat /tmp/sess.txt)" | grep completed'
```

Expected: still `completed`. The restart does not change a completed session's status.

### Step 6: Send a follow-up message to the completed session

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session send --id "$(cat /tmp/sess.txt)" --message "What was the secret number I told you?"'
```

Expected: exits 0. The server accepts the message on a `completed` session and spawns a
fresh harness that loads history from the DB.

### Step 7: Tail the session to see the second turn

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/sess.txt)"'
```

Expected: the tail replays the first run's events, then shows a second set of turn events
ending with `[done]`. The agent's second response must reference `4219` — it retrieved
this from the DB-reconstructed conversation history, not from any in-memory state (the
harness was cold-started after the server restart).

### Step 8: Confirm session is `completed` again

```bash
docker exec ns2-flow-18 bash -c 'cd /repo && ns2 session list --id "$(cat /tmp/sess.txt)" | grep completed'
```

Expected: `completed`. The session returned to terminal state after the second turn.

### Step 9: Verify `session send` on a `failed` session returns an error

```bash
docker exec ns2-flow-18 bash -c '
  cd /repo
  SESS=$(ns2 session new --message "test" )
  sleep 2
  # Force status to failed via admin PATCH
  curl -sf -X PATCH "http://localhost:9876/sessions/$SESS/status" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"failed\"}" > /dev/null
  ns2 session send --id "$SESS" --message "should fail"
  echo "Exit: $?"
'
```

Expected: a non-zero exit code (e.g., `Exit: 1`) and an error message on stderr indicating
the session cannot accept messages in its current state.

## Acceptance Criteria

- [ ] `session send` on a `completed` session returns 200 (not 4xx)
- [ ] A fresh harness is spawned that loads full turn history from SQLite
- [ ] The second turn's response references context from the first turn (proving DB
      reconstruction, not in-memory state)
- [ ] The session transitions back to `running` while the second turn is in progress
- [ ] The session returns to `completed` after the second turn finishes
- [ ] A `completed` session's status is not changed by a server restart (no orphan sweep
      for terminal sessions)
- [ ] `session send` on a `failed` session returns a non-2xx status

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.