---
targets:
  - crates/tools/src/**/*.rs
  - crates/harness/src/**/*.rs
severity: warning
verified: 2026-04-25T21:22:22Z
---

# Flow 03: Bash Tool

Verify Claude invokes the `bash` tool correctly and captures its output.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

## Fixture Setup

```bash
docker exec ns2-flow-03 bash /fixtures/init.sh
docker exec ns2-flow-03 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Create a session asking Claude to run a bash command with a unique marker

```bash
docker exec ns2-flow-03 bash -c 'cd /repo && ns2 session new --message "Please run the bash command: echo \"bash-tool-test-marker-\$(date +%s)\" and tell me the exact output you received." | tee /tmp/session1_id.txt && echo "Session created: $(cat /tmp/session1_id.txt)"'
```

Expected: a UUID printed alongside `Session created:`.

### Step 2: Tail the session and wait for completion

```bash
docker exec ns2-flow-03 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session1_id.txt)"'
```

The tail output should show:
- A `[tool: bash(...)]` line indicating the bash tool was invoked
- A `[result: ...]` line containing the output of the echo command (a string starting with `bash-tool-test-marker-` followed by a Unix timestamp)
- A final assistant response that quotes or references the marker string
- `[done]` at the end

Expected output shape:
```
[turn <uuid>]
[tool: bash({"command":"echo \"bash-tool-test-marker-<timestamp>\""})]
[turn <uuid>]
[result: bash-tool-test-marker-<timestamp>]
[turn <uuid>]
The output was: bash-tool-test-marker-<timestamp>
[done]
```

The exact phrasing of the assistant response varies, but it must contain the `bash-tool-test-marker-` string.

### Step 3: Verify session status

```bash
docker exec ns2-flow-03 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the session appears with status `completed`.

### Step 4: Create a second session to test `ls /`

```bash
docker exec ns2-flow-03 bash -c 'cd /repo && ns2 session new --message "Please run \"ls /\" using the bash tool and tell me what files and directories are listed there." | tee /tmp/session2_id.txt && echo "Session created: $(cat /tmp/session2_id.txt)"'
```

Expected: a second UUID printed alongside `Session created:`.

### Step 5: Tail the second session

```bash
docker exec ns2-flow-03 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session2_id.txt)"'
```

The tail output should show a `[tool: bash(...)]` line with `ls /`, a `[result: ...]` line listing directory contents, and Claude's response describing what it found. The result must include `repo` (which exists at `/repo`).

## Acceptance Criteria

- [ ] Claude invokes the `bash` tool (a `[tool: bash(...)]` line appears in session tail output)
- [ ] The bash command output appears in a `[result: ...]` line
- [ ] Claude's final response references the actual bash output (contains the `bash-tool-test-marker-` string)
- [ ] The session transitions to `completed`
- [ ] For the `ls /` session: the result includes `repo` and Claude describes the directory listing
- [ ] No panics or unhandled errors in server output