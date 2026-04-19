# Flow 05: Error Handling When Server Is Down

Verify that CLI commands produce clear, actionable error messages when the server is not running.

## Setup

Ensure the server is **not** running. Stop it if needed:

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
$NS2 server stop 2>/dev/null || true
```

Confirm the server is down:
```bash
curl -s http://127.0.0.1:9876/health 2>&1 || echo "Server not reachable (expected)"
```

Expected: connection refused error or `Server not reachable (expected)`.

## Steps

### Try `session list` with server down

```bash
$NS2 session list
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session new` with server down

```bash
$NS2 session new --message "hello"
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session tail` with server down

```bash
$NS2 session tail --id "00000000-0000-0000-0000-000000000000"
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Try `session send` with server down

```bash
$NS2 session send --id "00000000-0000-0000-0000-000000000000" --message "hi"
```

Expected output (exit code non-zero):
```
Error: server is not running. Start it with: ns2 server start
```

### Verify exit codes

```bash
$NS2 session list; echo "Exit code: $?"
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
