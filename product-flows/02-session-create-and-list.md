# Flow 02: Session Create and List

Create sessions and verify they appear in list output.

## Setup

1. Server must be running. If not, complete [01-server-lifecycle.md](01-server-lifecycle.md) first:
   ```bash
   source product-flows/setup.sh
   cd /tmp/ns2-test-repo
   $NS2 server start &
   sleep 1
   ```

## Steps

### Create a session with no message

```bash
$NS2 session new
```

Expected output:
```
Created session: unnamed (3f7a1c2d-0e4b-4a8f-9c1d-2b5e6f7a8b9c)
```

The session starts in `created` state (no message means no agent run).

### Create a session with a name

```bash
$NS2 session new --name "my-test-session"
```

Expected output:
```
Created session: my-test-session (7b2d3e4f-1a5c-4b9e-8d2f-3c6a7b8c9d0e)
```

### List all sessions

```bash
$NS2 session list
```

Expected output — a table showing both sessions:
```
id                                    name                  status      created_at
3f7a1c2d-0e4b-4a8f-9c1d-2b5e6f7a8b9c  unnamed               created     2024-01-01 00:00:00 UTC
7b2d3e4f-1a5c-4b9e-8d2f-3c6a7b8c9d0e  my-test-session       created     2024-01-01 00:00:01 UTC
```

### Filter by status

```bash
$NS2 session list --status created
```

Expected: both sessions (both are in `created` state).

```bash
$NS2 session list --status running
```

Expected: `No sessions found.` — no sessions are running.

## Acceptance Criteria

- [ ] `ns2 session new` prints `Created session: <name> (<uuid>)` and exits 0
- [ ] `ns2 session new --name <name>` stores the name; it appears in `session list` output
- [ ] `ns2 session list` shows all created sessions
- [ ] Sessions created with no message have status `created`
- [ ] `ns2 session list --status created` returns only created sessions
- [ ] `ns2 session list --status running` returns `No sessions found.` when none are running
- [ ] IDs in list output match the UUIDs from `session new` output
