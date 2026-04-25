---
targets:
  - crates/harness/src/**/*.rs
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
severity: warning
verified: 2026-04-25T11:26:14Z
---

# Flow 08: Multi-Turn Conversation

A user sends a follow-up message to a completed session and Claude responds with full prior context.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

## Fixture Setup

```bash
docker exec ns2-flow-08 bash /fixtures/init.sh
docker exec ns2-flow-08 bash /fixtures/start-server.sh
docker exec ns2-flow-08 bash /fixtures/seeded-files.sh
```

## Steps

### Step 1: Start a session asking Claude to read a file

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session new --message "Please read the file at /repo/multi-turn-test.txt and tell me what the magic number is." | tee /tmp/session_id.txt && echo "Session created: $(cat /tmp/session_id.txt)"'
```

Expected: a UUID printed alongside `Session created:`.

### Step 2: Tail the session and wait for completion

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

The output should show multiple turns (user message, tool call, tool result, final response) ending with `[done]`. Claude's response must include `7742`.

### Step 3: Verify the session is completed

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the session appears with status `completed`.

### Step 4: Send a follow-up message referencing the first answer

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session send --id "$(cat /tmp/session_id.txt)" --message "What was the magic number you found? Double it and tell me the result."'
```

Expected: command exits 0 with no error.

### Step 5: Tail the session again to see the second agent run

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

The tail output should first replay the first run's events (user message, tool call, tool result, first assistant response), then show a second set of `[turn ...]` events for the follow-up, and end with `[done]` again.

Claude's response to the follow-up must:
- Reference `7742` from the prior context (it should know the magic number without re-reading the file)
- State the doubled value: `15484`

Expected output shape:
```
[turn <uuid>]  ← first run user message
[turn <uuid>]  ← first run assistant tool call
[tool: read({"path":"/repo/multi-turn-test.txt"})]
[turn <uuid>]  ← first run tool result
[result: The magic number is: 7742]
[turn <uuid>]  ← first run assistant response
The magic number is 7742.
[done]
[turn <uuid>]  ← second run user follow-up
[turn <uuid>]  ← second run assistant response (no tool call needed)
The magic number was 7742. Doubled, that is 15484.
[done]
```

The exact phrasing varies. The key checks are that a second set of turn events appears after the first `[done]`, the second response contains `15484`, and no new `[tool: read(...)]` call appears in the second run.

### Step 6: Verify the session is completed again

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the same session still appears with status `completed`.

### Step 7: Re-tail with --turns 1 to replay only the final turn

```bash
docker exec ns2-flow-08 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)" --turns 1'
```

Expected: only the final assistant turn is replayed — no `[tool: read(...)]` line, no first-run content, just the last response containing `15484` followed by `[done]`.

## Acceptance Criteria

- [ ] First session run completes with `completed` status
- [ ] `session send` on a `completed` session returns 200, not 4xx
- [ ] The second run processes the follow-up message with full conversation history
- [ ] Claude's second response references `7742` (from context, not a new tool call)
- [ ] Claude's second response contains `15484` (7742 doubled)
- [ ] The session returns to `completed` after the second run
- [ ] `session tail` after the second run shows two sets of turn events ending in a single `[done]`
- [ ] `ns2 session tail` output includes `[tool: read(...)]` and `[result: ...]` lines for the first-run tool call only — the second run must not re-read the file
- [ ] `ns2 session tail --turns 1` replays only the final turn (no tool call, no first-run content)
- [ ] No panics or unhandled errors in server output