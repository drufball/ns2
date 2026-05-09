# GH#121 Smoke Test — Flow 03: Issue Lifecycle with Stop Tool

## Context

This test verifies GH#121 changes: the **stop tool**, **waiting status**, and **issue status following session status**.
Branch: `6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour`
Binary: `/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2`

**IMPORTANT**: Docker is not available. Run all commands directly on the host (no `docker exec` wrapper). Use the exact binary path above as `NS2`.

## Environment Setup

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY"

TEST_DIR="/tmp/ns2-gh121-flow03-$$"
mkdir -p "$TEST_DIR"
cd "$TEST_DIR"
git init .
git commit --allow-empty -m "init"

# Start the server
$NS2 server start --log-level info > /tmp/ns2-gh121-flow03-server.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
sleep 2

# Create agent WITH stop tool guidance
$NS2 --server http://localhost:9876 agent new \
  --name "swe" \
  --description "Software engineer agent" \
  --body "You are a software engineer. When asked to do something, do it concisely and confirm completion. IMPORTANT: When you are done, you MUST call the stop tool with status='complete' and a brief comment summarizing what you did."
```

## Steps

### Step 1: Create issue and verify open status

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
cd /tmp/ns2-gh121-flow03-$$  # or whatever the test dir was
ISSUE=$($NS2 --server http://localhost:9876 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World. Then call the stop tool with status=complete." --assignee swe)
echo "Issue: $ISSUE"
$NS2 --server http://localhost:9876 issue list --status open
```

Expected: 4-char issue ID, issue appears in `open` list.

### Step 2: Start the issue

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 issue start --id "$ISSUE"
```

Expected: session UUID printed, issue transitions to `running`.

### Step 3: Wait for issue completion

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 issue wait --id "$ISSUE"
echo "Exit code: $?"
```

Expected: command exits 0. GH#121: `issue wait` must terminate on `waiting` status too (not just `completed`/`failed`).

### Step 4: Check final issue status

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 issue list --status completed
$NS2 --server http://localhost:9876 issue list --status waiting
```

Expected: if agent called stop(complete), issue is in `completed` list. If agent did NOT call stop, issue is in `waiting` list.
Either outcome is acceptable, but the status must match exactly.

### Step 5: Verify `ns2 issue list --status waiting` filter works

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 issue list --status waiting
echo "Exit: $?"
```

Expected: command exits 0 (filter is valid), even if result set is empty.

### Step 6: Check for stop tool comment (if completed)

```bash
curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d.get('comments', [])
agent_comments = [c for c in comments if c.get('author') == 'swe']
print('Total comments:', len(comments))
print('Agent comments:', len(agent_comments))
if agent_comments:
    print('Latest agent comment:', agent_comments[-1]['body'][:100])
    print('OK - stop tool comment found')
else:
    print('INFO - no stop tool comment (session may be waiting, not completed)')
"
```

### Step 7: Test waiting status scenario — session without stop tool

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
# Create a second issue where the agent is less likely to call stop
ISSUE2=$($NS2 --server http://localhost:9876 issue new --title "Count to three" --body "Just say the numbers 1, 2, 3 on separate lines." --assignee swe)
$NS2 --server http://localhost:9876 issue start --id "$ISSUE2"
$NS2 --server http://localhost:9876 issue wait --id "$ISSUE2"
echo "Issue2 exit: $?"
$NS2 --server http://localhost:9876 issue list --status waiting
$NS2 --server http://localhost:9876 issue list --status completed
```

Expected: `issue wait` exits 0 regardless of whether final status is `waiting` or `completed`.

### Step 8: Complete and comment

```bash
NS2="/Users/drufball/.ns2/ns2/worktrees/6ofm-gh-121-add-stop-tool-remove-auto-complete-behaviour/target/release/ns2"
$NS2 --server http://localhost:9876 issue complete --id "$ISSUE" --comment "Verified: hello.txt created."
$NS2 --server http://localhost:9876 issue comment --id "$ISSUE" --body "Good work!" --author reviewer
```

Expected: both commands exit 0.

### Step 9: Check server log for panics

```bash
cat /tmp/ns2-gh121-flow03-server.log | grep -i "panic\|PANIC\|thread.*panicked" | head -20
echo "---"
cat /tmp/ns2-gh121-flow03-server.log | grep -iE "ERROR|WARN" | head -20
```

Expected: no panics.

## Acceptance Criteria

- [ ] `ns2 issue new` prints a 4-character issue ID
- [ ] New issues start with status `open` and auto-generated branch slug
- [ ] `ns2 issue start` creates a session linked to the issue (status → `running`)
- [ ] `ns2 issue wait` terminates when issue reaches `waiting` OR `completed` OR `failed` (GH#121)
- [ ] `ns2 issue wait` exits 0 on `waiting` status (not just `completed`)
- [ ] `ns2 issue list --status waiting` is a valid filter and works without error
- [ ] `ns2 issue list --status completed` is a valid filter and works without error
- [ ] Issue status follows session status (`waiting` session → `waiting` issue, `completed` session → `completed` issue)
- [ ] When agent calls `stop(complete, comment)`, the comment appears with `author == assignee`
- [ ] `ns2 issue complete` adds a manual summary comment
- [ ] `ns2 issue comment` adds comments with the specified author
- [ ] No panics or unhandled errors in server output
