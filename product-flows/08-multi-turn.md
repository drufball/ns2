# Flow 08: Multi-Turn Conversation

A user sends a follow-up message to a completed session and Claude responds with full prior context.

## Setup

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
```

Place a `.env` file in the repo root (next to `Cargo.toml`) with your key:
```
ANTHROPIC_API_KEY=sk-ant-...
```

Create a test file for Claude to read:
```bash
echo "The magic number is: 7742" > /tmp/ns2-multi-turn-test.txt
```

Start the server:
```bash
$NS2 server start &
```

## Steps

### Step 1: Start a session asking Claude to read a file

```bash
SESSION_ID=$($NS2 session new --message "Please read the file at /tmp/ns2-multi-turn-test.txt and tell me what the magic number is.")
echo "Session: $SESSION_ID"
```

Expected: a UUID printed and stored in `SESSION_ID`.

### Step 2: Tail the session and wait for completion

```bash
$NS2 session tail --id "$SESSION_ID"
```

The output should show multiple turns (user message, tool call, tool result, final response) ending with `[done]`. Claude's response must include `7742`.

### Step 3: Verify the session is completed

```bash
$NS2 session list --status completed
```

Expected: the session appears with status `completed`.

### Step 4: Send a follow-up message referencing the first answer

```bash
$NS2 session send --id "$SESSION_ID" --message "What was the magic number you found? Double it and tell me the result."
```

Expected: 200 OK (no error).

### Step 5: Tail the session again to see the second agent run

```bash
$NS2 session tail --id "$SESSION_ID"
```

The tail output should first replay the first run's events (user message, tool call, tool result, first assistant response), then show a second set of `[turn ...]` events for the follow-up, and end with `[done]` again.

Claude's response to the follow-up must:
- Reference `7742` from the prior context (it should know the magic number without re-reading the file)
- State the doubled value: `15484`

### Step 6: Verify the session is completed again

```bash
$NS2 session list --status completed
```

Expected: the same session still appears with status `completed`.

## Expected tail output (second run)

```
[turn <uuid>]  ← first run user message
[turn <uuid>]  ← first run assistant tool call
[turn <uuid>]  ← first run tool result
[turn <uuid>]  ← first run assistant response
The magic number is 7742.
[done]
[turn <uuid>]  ← second run user follow-up
[turn <uuid>]  ← second run assistant response (no tool call needed)
The magic number was 7742. Doubled, that is 15484.
[done]
```

The exact phrasing varies. The key checks are that a second set of turn events appears and the answer references prior context without a new tool call.

## Acceptance Criteria

- [ ] First session run completes with `completed` status
- [ ] `session send` on a `completed` session returns 200, not 4xx
- [ ] The second run processes the follow-up message with full conversation history
- [ ] Claude's second response references `7742` (from context, not a new tool call)
- [ ] The session returns to `completed` after the second run
- [ ] `session tail` after the second run shows two sets of turn events (two `[done]` markers or equivalent)
- [ ] No panics or unhandled errors in server output

## Cleanup

```bash
rm -f /tmp/ns2-multi-turn-test.txt
bash product-flows/cleanup.sh
```
