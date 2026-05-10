
# Flow 01: Hello World (Real Claude API)

Full session lifecycle using the real Anthropic API. This is the most basic end-to-end smoke test — it verifies the harness connects to Anthropic, processes a real response, and stores it correctly.

## Setup

```bash
/fixtures/init-git-repo.sh
/fixtures/copy-env.sh
cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &
sleep 3
```

## Steps

### Create a session with a message

```bash
SESSION=$(ns2 session new --message "hello")
echo "Session: $SESSION"
```

Expected: a UUID printed to stdout.

### Tail the session

```bash
ns2 session tail --id "$SESSION"
```

Streams events as Claude responds. Response time depends on API latency — typically 2–10 seconds for a short reply.

Expected output shape:
```
[assistant] Hello! How can I help you today?
Session waiting.
```

The exact wording varies. It must be coherent natural language — not the stub string "I'm a stub assistant."

### Verify session status

```bash
ns2 session list --status waiting
```

Expected: the session appears with status `waiting`.

### Re-tail to confirm stored content replays

```bash
ns2 session tail --id "$SESSION"
```

Re-tailing a waiting session replays stored content. Confirm the response reads like a real Claude reply.

## Acceptance Criteria

- [ ] `ns2 server start` loads `ANTHROPIC_API_KEY` from the `.env` file
- [ ] `ns2 session new --message "hello"` creates a session (note: the `running` state is transient for short responses and may not be observable via polling — verify via `ns2 session tail` which confirms the session ran)
- [ ] `ns2 session tail` streams real text from the Anthropic API
- [ ] The response is coherent natural language (not "I'm a stub assistant.")
- [ ] The session transitions to `waiting` after the response is fully streamed
- [ ] `ns2 session list --status waiting` shows the session
- [ ] Re-tailing a waiting session replays the stored content identically
- [ ] No panics, stack traces, or unhandled errors in server output
