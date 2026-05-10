
# Flow 03: Issue Lifecycle

Create an issue, assign it to an agent, set status to in_progress to automatically start execution, wait for completion, and mark it done. This is the primary orchestration smoke test.

## Setup

```bash
/fixtures/init-git-repo.sh
/fixtures/copy-env.sh
cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &
sleep 3
/fixtures/create-swe-agent.sh
```

## Steps

### Step 1: Create and immediately start an issue with --wait

```bash
ISSUE=$(ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe --status in_progress --wait | tail -1)
echo "Issue: $ISSUE"
```

Expected: the command blocks until the issue reaches a terminal state. `--wait` prints a status line (`<id>  <status>`) to stdout before the final ID line, so `tail -1` extracts just the 4-character issue ID. The `--wait` flag requires `--status in_progress`.

Note: `run_wait` (issue.rs:540–542) prints `{id}  {status}` to stdout when done, then `run_new` (issue.rs:112) prints `{issue_id}` — two stdout lines total. `tail -1` captures only the bare ID.

### Step 2: Verify the issue exists with status open (without --wait)

```bash
ISSUE2=$(ns2 issue new --title "Another task" --body "Do something" --assignee swe)
ns2 issue list --status open
```

Expected: a table showing the issue with status `open`, assignee `swe`, and auto-generated branch `<id>-another-task`.

### Step 3: --wait without --status in_progress should error

```bash
ns2 issue new --title "Test" --body "Test" --wait 2>&1 || true
```

Expected: exits non-zero with error message: `--wait requires --status in_progress`.

### Step 4: --watch streams events for the new issue

**[KNOWN BROKEN]** The `--watch` flag is partially implemented but does not work as expected when used without `--wait`.

```bash
ns2 issue new --title "Watch test" --body "Test" --status in_progress --watch --wait
# events are printed to stdout as they arrive
```

### Step 5: Verify issue status is completed

```bash
ns2 issue list --status completed
```

Expected: the issue shows with status `completed`.

### Step 6: Verify the agent posted a comment via the stop tool

```bash
ns2 issue show --id "$ISSUE" --json | python3 -c "
import sys, json
d = json.load(sys.stdin)
agent_comments = [c for c in d['comments'] if c['author'] == 'swe']
print('Agent comments:', len(agent_comments))
print('OK' if agent_comments else 'FAIL — no agent comment found')
"
```

Expected: `OK` — the agent explicitly called the stop tool with a comment, which is posted with `author == "swe"`.

### Step 7: Mark an open issue done with a completion comment

Note: `ns2 issue complete` errors if called on an already-completed issue (`$ISSUE` was completed by the agent's stop tool call). Use it on `$ISSUE2` which was created but never started.

```bash
ns2 issue complete --id "$ISSUE2" --comment "Decided not to proceed with this task."
```

Expected: command exits 0.

### Step 8: Add a regular comment

```bash
ns2 issue comment --id "$ISSUE" --body "Good work!" --author reviewer
```

Expected: command exits 0.

## Acceptance Criteria

- [ ] `ns2 issue new` prints a 4-character issue ID to stdout
- [ ] New issues start with status `open` and an auto-generated branch slug
- [ ] `ns2 issue new --status in_progress` auto-starts the issue (spawns the agent harness)
- [ ] `ns2 issue new --status in_progress --wait` blocks until the issue reaches a terminal state, then prints the ID
- [ ] `ns2 issue new --wait` without `--status in_progress` exits non-zero with error: `--wait requires --status in_progress`
- [ ] `ns2 issue new --watch --status in_progress` prints SSE events as the issue progresses (requires `--wait` to be passed for now)
- [ ] Setting status to `in_progress` automatically creates a session and starts execution
- [ ] The session uses the issue's assignee as the agent type
- [ ] `ns2 issue wait` blocks until the issue reaches a terminal state and exits 0
- [ ] When the agent calls `stop(status="complete", comment="...")`, the comment is posted and the issue becomes `completed`
- [ ] When the agent calls `stop(status="waiting")`, the issue becomes `waiting`
- [ ] When the session ends without the agent calling the stop tool, the issue becomes `waiting` with no auto-comment
- [ ] `ns2 issue complete` adds a manual summary comment
- [ ] `ns2 issue comment` adds comments with the specified author
- [ ] No panics or unhandled errors in server output
