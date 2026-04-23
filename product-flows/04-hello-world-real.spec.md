---
targets:
  - crates/harness/src/**/*.rs
  - crates/anthropic/src/**/*.rs
  - crates/server/src/**/*.rs
severity: warning
verified: 2026-04-22T19:19:38Z
---


# Flow 04: Hello World (Real Claude API)

Full session lifecycle using the real Anthropic API.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

## Fixture Setup

```bash
docker exec ns2-flow-04 bash /fixtures/init.sh
docker exec ns2-flow-04 bash /fixtures/start-server.sh
```

## Steps

### Create a session with a message

```bash
docker exec ns2-flow-04 bash -c 'cd /repo && ns2 session new --message "hello" | tee /tmp/session_id.txt && echo "Session created: $(cat /tmp/session_id.txt)"'
```

Expected: a UUID printed alongside `Session created:`.

### Tail the session

```bash
docker exec ns2-flow-04 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

The command streams events as Claude responds. Response time depends on API latency — typically 2–10 seconds for a short reply.

Expected output shape:
```
[assistant] Hello! How can I help you today?
Session completed.
```

The exact wording comes from Claude and will vary. It must be a coherent English sentence — not the stub string "I'm a stub assistant."

### Verify session status

```bash
docker exec ns2-flow-04 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the session appears with status `completed`.

### Re-tail to confirm stored content replays correctly

```bash
docker exec ns2-flow-04 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

Re-tailing a completed session replays the stored content. Confirm the response reads like a real Claude reply, not the hardcoded stub.

## Acceptance Criteria

- [ ] `ns2 server start` picks up `ANTHROPIC_API_KEY` from the `.env` file
- [ ] `ns2 session new --message "hello"` creates a session that transitions to `running`
- [ ] `ns2 session tail` streams real text from the Anthropic API
- [ ] The response is coherent natural language (not the stub string "I'm a stub assistant.")
- [ ] The session transitions to `completed` after the response is fully streamed
- [ ] `ns2 session list --status completed` shows the session
- [ ] Re-tailing a completed session replays the stored content identically
- [ ] No panics, stack traces, or unhandled errors in server output

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.