# Flow 04: Hello World (Real Claude API)

Full session lifecycle using the real Anthropic API. Requires a valid `ANTHROPIC_API_KEY`.

## Setup

Source the setup script so `ANTHROPIC_API_KEY` is loaded from `.env`:

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
```

Confirm the key is set:
```bash
echo "${ANTHROPIC_API_KEY:+set (length ${#ANTHROPIC_API_KEY})}"
```
Expected: `set (length 108)` — or whatever length your key is. Must not print `not set`.

Start the server:
```bash
$NS2 server start &
```

## Steps

### Create a session with a message

```bash
SESSION_ID=$($NS2 session new --message "hello")
echo "Session: $SESSION_ID"
```

Expected: a UUID printed and stored in `SESSION_ID`.

### Tail the session

```bash
$NS2 session tail --id "$SESSION_ID"
```

The command streams events as Claude responds. Response time depends on API latency — typically 2–10 seconds for a short reply.

### Expected output

```
[assistant] Hello! How can I help you today?
Session completed.
```

The exact wording comes from Claude and will vary. It must be a coherent English sentence — not the stub string "I'm a stub assistant."

### Verify session status

```bash
$NS2 session list --status completed
```

Expected: the session ID appears with status `completed`.

### Optional: confirm it was not the stub

```bash
$NS2 session tail --id "$SESSION_ID"
```

Re-tailing a completed session replays the stored content. Confirm the response reads like a real Claude reply, not the hardcoded stub.

## Acceptance Criteria

- [ ] `ns2 server start` picks up `ANTHROPIC_API_KEY` from the environment
- [ ] `ns2 session new --message "hello"` creates a session and it transitions to `running`
- [ ] `ns2 session tail` streams real text from the Anthropic API
- [ ] The response is coherent natural language (not the stub string)
- [ ] The session transitions to `completed` after the response is fully streamed
- [ ] `ns2 session list --status completed` shows the session
- [ ] No panics, stack traces, or unhandled errors in server output
