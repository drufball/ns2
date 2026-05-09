
# Flow 03: Issue Lifecycle

Create an issue, assign it to an agent, set status to in_progress to automatically start execution, wait for completion, and mark it done. This is the primary orchestration smoke test.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

```bash
git init /tmp/ns2-smoke && cd /tmp/ns2-smoke
git commit --allow-empty -m "init"
ns2 server start
ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion. When you are done, call the stop tool with status='complete' and a brief comment summarizing what you did."
```

## Fixture Setup

```bash
docker exec ns2-flow-03 bash -c 'mkdir -p /tmp/ns2-smoke && git -C /tmp/ns2-smoke init && git -C /tmp/ns2-smoke commit --allow-empty -m "init"'
docker exec -d ns2-flow-03 bash -c 'set -a; . /tmp/ns2-host.env; set +a; cd /tmp/ns2-smoke && ns2 server start'
sleep 3
docker exec ns2-flow-03 bash -c 'cd /tmp/ns2-smoke && ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion. When you are done, call the stop tool with status='"'"'complete'"'"' and a brief comment summarizing what you did."'
```

## Steps

### Step 1: Create and immediately start an issue with --wait

```bash
ISSUE=$(ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe --status in_progress --wait)
echo "Issue: $ISSUE"
```

Expected: the command blocks until the issue reaches a terminal state, then prints the issue ID to stdout (e.g., `a1b2`). The `--wait` flag requires `--status in_progress`.

### Step 1b (alternative): Create separately then wait

```bash
ISSUE=$(ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe)
ns2 issue set-status --id "$ISSUE" --status in_progress
ns2 issue wait --id "$ISSUE"
echo "Issue: $ISSUE"
```

Expected: same result — blocks until completion.

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

```bash
WATCH_ISSUE=$(ns2 issue new --title "Watch test" --body "Test" --status in_progress --watch &)
# events are printed to stdout as they arrive
```

Expected: SSE events (status_changed, comment_added) are printed to stdout as the issue progresses. Works with any status.

### Step 5: Verify issue status is completed

```bash
ns2 issue list --status completed
```

Expected: the issue shows with status `completed`.

### Step 6: Verify the agent posted a comment via the stop tool

```bash
curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d['comments']
agent_comments = [c for c in comments if c['author'] == 'swe']
print('Agent comments:', len(agent_comments))
print('OK' if agent_comments else 'FAIL — no agent comment found')
"
```

Expected: `OK` — the agent explicitly called the stop tool with a comment, which is posted with `author == "swe"`.

### Step 7: Mark it done with a completion comment

```bash
ns2 issue complete --id "$ISSUE" --comment "Verified: hello.txt created with correct content."
```

Expected: command exits 0.

### Step 8: Add a regular comment

```bash
ns2 issue comment --id "$ISSUE" --body "Good work!" --author reviewer
```

Expected: command exits 0.

## Waiting Status

If an agent's session ends without calling the stop tool, the issue transitions to
`waiting` (not `completed`). This lets operators know the agent stopped unexpectedly
and the issue needs attention.

```bash
# An issue linked to a session that ended without stop tool → status = waiting
ns2 issue list --status waiting
```

## Acceptance Criteria

- [ ] `ns2 issue new` prints a 4-character issue ID to stdout
- [ ] New issues start with status `open` and an auto-generated branch slug
- [ ] `ns2 issue new --status in_progress` auto-starts the issue (spawns the agent harness)
- [ ] `ns2 issue new --status in_progress --wait` blocks until the issue reaches a terminal state, then prints the ID
- [ ] `ns2 issue new --wait` without `--status in_progress` exits non-zero with error: `--wait requires --status in_progress`
- [ ] `ns2 issue new --watch` (any status) prints SSE events to stdout as the issue progresses
- [ ] `ns2 issue new --status in_progress --watch` starts the issue and streams events simultaneously
- [ ] Setting status to `in_progress` automatically creates a session and starts execution
- [ ] The session uses the issue's assignee as the agent type
- [ ] `ns2 issue wait` blocks until the issue reaches a terminal state and exits 0
- [ ] When the agent calls `stop(status="complete", comment="...")`, the comment is posted and the issue becomes `completed`
- [ ] When the agent calls `stop(status="waiting")`, the issue becomes `waiting`
- [ ] When the session ends without the agent calling the stop tool, the issue becomes `waiting` with no auto-comment
- [ ] `ns2 issue complete` adds a manual summary comment
- [ ] `ns2 issue comment` adds comments with the specified author
- [ ] Issue status transitions: open → running (via in_progress) → completed (via stop tool) or open → running → waiting (no stop tool)
- [ ] If the issue's session was in an error state when in_progress is set, the old session is removed before creating a new one
- [ ] The ns2 orchestration skill and product-manager agent use `--wait` instead of the 2-step `set-status` + `issue wait` pattern
- [ ] No panics or unhandled errors in server output
