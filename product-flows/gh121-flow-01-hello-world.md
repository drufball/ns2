# GH#121 Smoke Test — Flow 01: Hello World (Real Claude API)

## Context

This test verifies GH#121 changes: the **stop tool** and **waiting status**.
Branch: `6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour`
Binary: `/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2`

**IMPORTANT**: Docker is not available. Run all commands directly on the host (no `docker exec` wrapper). Use the exact binary path above as `NS2`.

## Environment Setup

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"

# Use a fresh temp dir to avoid conflicts
TEST_DIR="/tmp/ns2-gh121-flow01-$$"
mkdir -p "$TEST_DIR"
cd "$TEST_DIR"
git init .
git commit --allow-empty -m "init"

# Start server with the feature-branch binary, log to file
$NS2 server start --log-level info > /tmp/ns2-gh121-flow01-server.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
sleep 2

# Verify server is up
$NS2 --server http://localhost:9876 session list 2>&1 | head -5
```

## Steps

### Step 1: Create a session with --wait

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 session new --message "Say hello in exactly one sentence." --wait
```

Expected: exits 0, outputs UUID + final assistant turn with real Claude text (not "I'm a stub assistant.").

### Step 2: Check session status — NEW BEHAVIOUR

With GH#121, a session that completes WITHOUT calling the stop tool ends as **`waiting`**, NOT `completed`.
A bare `session new --message` uses the default (no agent), so no stop tool is called.

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 session list --status waiting
```

Expected: the session appears with status `waiting`.

```bash
$NS2 --server http://localhost:9876 session list --status completed
```

Expected: the session does NOT appear (it's `waiting`, not `completed`).

### Step 3: Filter by waiting status works

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 session list --status waiting
```

Expected: rows present, `--status waiting` filter works without error.

### Step 4: Tail the session

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
SESSION=$($NS2 --server http://localhost:9876 session new --message "Say hello in one sentence." 2>&1 | head -1)
echo "Session: $SESSION"
sleep 8
$NS2 --server http://localhost:9876 session tail --id "$SESSION"
```

Expected: coherent natural language response, ends with "Session waiting." or similar (not "Session completed." under GH#121 behaviour).

### Step 5: Cleanup

```bash
kill $SERVER_PID 2>/dev/null || true
cat /tmp/ns2-gh121-flow01-server.log | grep -i "panic\|error\|WARN" | head -20
```

Expected: no panics in server log.

## Acceptance Criteria

- [ ] Binary from feature branch starts a server without error
- [ ] `ns2 session new --message "..." --wait` exits 0 and outputs real Claude text
- [ ] Without stop tool, session ends with status `waiting` (NOT `completed`) — GH#121 key behaviour
- [ ] `ns2 session list --status waiting` works and shows the session
- [ ] `ns2 session list --status completed` does NOT show the waiting session
- [ ] Session tail output is coherent natural language
- [ ] No panics or unhandled errors in server output
