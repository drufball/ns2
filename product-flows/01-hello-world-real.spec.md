---
targets:
  - crates/harness/src/**/*.rs
  - crates/anthropic/src/**/*.rs
  - crates/server/src/**/*.rs
severity: warning
---

# Flow 01: Hello World (Real Claude API)

Full session lifecycle using the real Anthropic API. This is the most basic end-to-end smoke test — it verifies the harness connects to Anthropic, processes a real response, and stores it correctly.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

```bash
# In a temp directory with a git repo:
git init /tmp/ns2-smoke && cd /tmp/ns2-smoke
git commit --allow-empty -m "init"
ns2 server start
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
ns2 session list --status completed
```

Expected: the session appears with status `completed`.

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
- [ ] The session transitions to `completed` after the response is fully streamed
- [ ] `ns2 session list --status completed` shows the session
- [ ] Re-tailing a completed session replays the stored content identically
- [ ] No panics, stack traces, or unhandled errors in server output
