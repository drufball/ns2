---
targets:
  - crates/server/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-25T21:22:22Z
---

# Flow 01: Server Lifecycle

Start and stop the ns2 server; verify it responds to health checks.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-01 bash /fixtures/init.sh
```

## Steps

### Start the server

```bash
docker exec -d ns2-flow-01 bash -c 'cd /repo && ns2 server start'
sleep 1
```

### Verify the health endpoint

```bash
docker exec ns2-flow-01 bash -c 'curl -s http://127.0.0.1:9876/health && echo'
```

Expected output:
```
{"status":"ok"}
```

### Verify the PID file exists

```bash
docker exec ns2-flow-01 bash -c 'cat /root/.ns2/repo/server-9876.pid'
```

Expected: the PID of the server process (a positive integer).

### Stop the server

```bash
docker exec ns2-flow-01 bash -c 'cd /repo && ns2 server stop'
```

Expected output:
```
Server stopped (pid 12345)
```

(The actual PID will differ.)

### Verify the server is stopped

```bash
docker exec ns2-flow-01 bash -c 'curl -s http://127.0.0.1:9876/health 2>&1 || echo "Server not reachable (expected)"'
```

Expected: connection refused error or `Server not reachable (expected)` — not a JSON response.

### Verify the PID file is removed

```bash
docker exec ns2-flow-01 bash -c 'ls /root/.ns2/repo/server-9876.pid 2>/dev/null || echo "PID file removed (expected)"'
```

Expected: `PID file removed (expected)`.

## Acceptance Criteria

- [ ] `ns2 server start` exits cleanly and the process is listening on port 9876
- [ ] `GET /health` returns `{"status":"ok"}` with HTTP 200
- [ ] A PID file is written to `/root/.ns2/repo/server-9876.pid`
- [ ] `ns2 server stop` prints `Server stopped (pid <N>)` and terminates the server process
- [ ] After stop, port 9876 refuses connections
- [ ] After stop, the PID file is removed