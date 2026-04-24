---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-24T15:24:20Z
---

# Flow 02: Session Create and List

Create sessions and verify they appear in list output.

## Prerequisites

No API key required.

## Fixture Setup

```bash
docker exec ns2-flow-02 bash /fixtures/init.sh
docker exec ns2-flow-02 bash /fixtures/start-server.sh
```

## Steps

### Create a session with no message

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session new'
```

Expected output:
```
Created session: unnamed (<uuid>)
```

The session starts in `created` state (no message means no agent run).

### Create a session with a name

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session new --name "my-test-session"'
```

Expected output:
```
Created session: my-test-session (<uuid>)
```

### List all sessions

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session list'
```

Expected output — a table showing both sessions:
```
id                                    name                  status      created_at
<uuid>                                unnamed               created     2024-01-01 00:00:00 UTC
<uuid>                                my-test-session       created     2024-01-01 00:00:01 UTC
```

Both sessions must appear with status `created`.

### Filter by status — created

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session list --status created'
```

Expected: both sessions (both are in `created` state).

### Filter by status — running

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session list --status running'
```

Expected: `No sessions found.` — no sessions are running.

### Filter by session ID

```bash
docker exec ns2-flow-02 bash -c 'cd /repo && ns2 session list --id "$(ns2 session list | tail -1 | awk '\''{print $1}'\'')"'
```

Expected: only the single session row matching that UUID is shown. The output is still a table with headers, but contains exactly one data row.

## Acceptance Criteria

- [ ] `ns2 session new` prints `Created session: <name> (<uuid>)` and exits 0
- [ ] `ns2 session new --name <name>` stores the name; it appears in `session list` output
- [ ] `ns2 session list` shows all created sessions in a formatted table
- [ ] Sessions created with no message have status `created`
- [ ] `ns2 session list --status created` returns only `created` sessions
- [ ] `ns2 session list --status running` returns `No sessions found.` when none are running
- [ ] `ns2 session list --id <uuid>` returns exactly the session matching that UUID
- [ ] IDs in list output match the UUIDs printed by `session new`