# Flow 01: Server Lifecycle

Start and stop the ns2 server; verify it responds to health checks.

## Setup

1. Run the setup script from the worktree root:
   ```bash
   source product-flows/setup.sh
   ```
2. Change into the test repo so git root detection resolves to `ns2-test-repo`:
   ```bash
   cd /tmp/ns2-test-repo
   ```

## Steps

### Start the server

Start the server in the background (or open a second terminal):

```bash
$NS2 server start &
SERVER_PID=$!
```

### Verify the health endpoint

```bash
curl -s http://127.0.0.1:9876/health && echo
```

Expected output:
```
{"status":"ok"}
```

The `&& echo` adds a trailing newline. Without it, zsh prints a `%` after the JSON to indicate no newline — that's a zsh display convention, not an error.

### Verify the PID file exists

```bash
cat ~/.ns2/ns2-test-repo/server-9876.pid
```

Expected: the PID of the server process (a positive integer).

### Stop the server

```bash
$NS2 server stop
```

Expected output:
```
Server stopped (pid 12345)
```

### Verify the server is stopped

```bash
curl -s http://127.0.0.1:9876/health || echo "Server is not running (expected)"
```

Expected: connection refused error or the echo message — not a JSON response.

### Verify the PID file is removed

```bash
ls ~/.ns2/ns2-test-repo/server-9876.pid 2>/dev/null || echo "PID file removed (expected)"
```

## Acceptance Criteria

- [ ] `ns2 server start` exits cleanly and the process is listening on port 9876
- [ ] `GET /health` returns `{"status":"ok"}` with HTTP 200
- [ ] A PID file is written to `~/.ns2/ns2-test-repo/server-9876.pid`
- [ ] `ns2 server stop` terminates the server process
- [ ] After stop, port 9876 refuses connections
- [ ] After stop, the PID file is removed
