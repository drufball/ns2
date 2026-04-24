---
targets:
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-24T15:24:20Z
---

# Flow 05: Error Handling When Server Is Down

Verify that CLI commands produce clear, actionable error messages when the server is not running.

## Prerequisites

No API key required. The server must NOT be started — this flow tests behavior with no running server.

## Fixture Setup

```bash
docker exec ns2-flow-05 bash /fixtures/init.sh
```

The server is intentionally not started.

## Steps

### Confirm the server is not reachable

```bash
docker exec ns2-flow-05 bash -c 'curl -s http://127.0.0.1:9876/health 2>&1 || echo "Server not reachable (expected)"'
```

Expected: connection refused error or `Server not reachable (expected)`.

### Try `session list` with server down

```bash
docker exec ns2-flow-05 bash -c 'cd /repo && ns2 session list'
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session new` with server down

```bash
docker exec ns2-flow-05 bash -c 'cd /repo && ns2 session new --message "hello"'
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session tail` with server down

```bash
docker exec ns2-flow-05 bash -c 'cd /repo && ns2 session tail --id "00000000-0000-0000-0000-000000000000"'
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session send` with server down

```bash
docker exec ns2-flow-05 bash -c 'cd /repo && ns2 session send --id "00000000-0000-0000-0000-000000000000" --message "hi"'
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Verify exit codes

```bash
docker exec ns2-flow-05 bash -c 'cd /repo && ns2 session list; echo "Exit code: $?"'
```

Expected: `Exit code: 1` (or any non-zero value).

## Acceptance Criteria

- [ ] `ns2 session list` exits with a non-zero code when server is down
- [ ] `ns2 session new` exits with a non-zero code when server is down
- [ ] `ns2 session tail` exits with a non-zero code when server is down
- [ ] `ns2 session send` exits with a non-zero code when server is down
- [ ] All error messages are on a single line starting with `Error:`
- [ ] Error messages suggest the fix (`ns2 server start`)
- [ ] No Rust panics, backtraces, or `unwrap` failure output is visible
- [ ] No JSON error blobs or HTTP response bodies are printed raw to the user