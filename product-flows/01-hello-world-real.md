
# Flow 01: Hello World (Real Claude API)

Full session lifecycle using the real Anthropic API. This is the most basic end-to-end smoke test — it verifies the harness connects to Anthropic, processes a real response, and stores it correctly.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

Run each command via `docker exec ns2-flow-01 bash -c '...'`:

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

### (Alternative) Create a session with --wait

```bash
ns2 session new --message "hello" --wait
```

Expected: the command blocks until Claude responds and exits 0. Output is the session UUID followed by the final assistant turn only.

### Tail the session

```bash
ns2 session tail --id "$SESSION"
```

Streams events as Claude responds. Response time depends on API latency — typically 2–10 seconds for a short reply.

Expected output shape:
```
[assistant] Hello! How can I help you today?
Session completed.
```

The exact wording varies. It must be coherent natural language — not the stub string "I'm a stub assistant."

### Verify session status

```bash
ns2 session list --status waiting
```

Expected: the session appears with status `waiting`. Sessions transition to `waiting` after Claude responds — the stop tool controls issue status, not session status. Sessions do not reach `completed`.

### Re-tail to confirm stored content replays

```bash
ns2 session tail --id "$SESSION"
```

Re-tailing a completed session replays stored content. Confirm the response reads like a real Claude reply.

## Acceptance Criteria

- [ ] `ns2 server start` picks up `ANTHROPIC_API_KEY` from the environment
- [ ] `ns2 session new --message "hello"` creates a session that transitions to `running`
- [ ] `ns2 session new --message "hello" --wait` blocks until completion and exits 0
- [ ] `ns2 session tail` streams real text from the Anthropic API
- [ ] The response is coherent natural language (not "I'm a stub assistant.")
- [ ] The session transitions to `waiting` after the response is fully streamed (sessions do not reach `completed`; the stop tool controls issue status)
- [ ] `ns2 session list --status waiting` shows the session
- [ ] Re-tailing a waiting session replays the stored content identically
- [ ] No panics, stack traces, or unhandled errors in server output
