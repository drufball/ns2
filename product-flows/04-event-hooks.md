# Flow 04: Event Hooks & Subscriptions

Create internal hooks that react to issue state changes and deliver notifications without blocking. This is the primary orchestration smoke test for the hook system.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

```bash
git init /tmp/ns2-smoke && cd /tmp/ns2-smoke
git commit --allow-empty -m "init"
ns2 server start
ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion."
```

## Fixture Setup

```bash
docker exec ns2-flow-04 bash -c 'mkdir -p /tmp/ns2-smoke && git -C /tmp/ns2-smoke init && git -C /tmp/ns2-smoke commit --allow-empty -m "init"'
docker exec -d ns2-flow-04 bash -c 'set -a; . /tmp/ns2-host.env; set +a; cd /tmp/ns2-smoke && ns2 server start'
sleep 3
docker exec ns2-flow-04 bash -c 'cd /tmp/ns2-smoke && ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion."'
```

## Steps

### Step 1: Create a watcher issue

```bash
WATCHER=$(ns2 issue new --title "Watcher" --body "Watch for notifications")
echo "Watcher: $WATCHER"
```

Expected: a 4-character issue ID printed to stdout.

### Step 2: Create a work issue

```bash
WORK=$(ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe)
echo "Work: $WORK"
```

Expected: a 4-character issue ID printed to stdout.

### Step 3: Subscribe the watcher issue to the work issue's events

```bash
HOOK=$(ns2 issue subscribe --id "$WORK" --deliver-to "issue:$WATCHER")
echo "Hook created: $HOOK"
```

Expected: a 4-character hook ID printed to stdout; a hook is created in the system.

### Step 4: Verify the hook exists

```bash
ns2 hook list
```

Expected: a table showing the hook with the work issue ID in its name/filter, enabled=true.

### Step 5: Start the work issue and wait for completion

```bash
ns2 issue start --id "$WORK"
ns2 issue wait --id "$WORK"
```

Expected: exits 0 when the work issue completes.

### Step 6: Verify the watcher received a notification comment

```bash
sleep 2  # allow hook delivery to complete

curl -sf "http://localhost:9876/issues/$WATCHER" | python3 -c "
import sys, json, os
d = json.load(sys.stdin)
work = os.environ.get('WORK', '')
comments = [c for c in d['comments'] if work in c['body'] or 'completed' in c['body'].lower() or 'running' in c['body'].lower()]
print('Notification comments:', len(comments))
print('OK' if comments else 'FAIL — no notification comment found')
"
```

Expected: `OK` — the hook fired when the work issue changed status and posted a comment to the watcher issue.

### Step 7: Stream events via the global SSE endpoint

```bash
timeout 3 curl -sN "http://localhost:9876/events" | head -5 || true
```

Expected: SSE data lines are printed (stream is live, may be empty if no active sessions).

### Step 8: Stream issue-specific events

```bash
ISSUE2=$(ns2 issue new --title "Test issue events" --body "test")
timeout 5 curl -sN "http://localhost:9876/events?issue_id=$ISSUE2" &
sleep 1
ns2 issue start --id "$ISSUE2" 2>/dev/null || true
sleep 2
```

Expected: SSE events appear for that issue when it starts (status_changed).

### Step 9: Hook lifecycle — disable, enable, delete

```bash
ns2 hook disable --id "$HOOK"
ns2 hook list
# Expected: hook shows enabled=false

ns2 hook enable --id "$HOOK"
ns2 hook list
# Expected: hook shows enabled=true

ns2 hook delete --id "$HOOK"
ns2 hook list
# Expected: hook no longer appears
```

## Acceptance Criteria

- [ ] `ns2 issue subscribe` creates an internal hook visible via `ns2 hook list`
- [ ] When a subscribed issue changes status, a notification comment is posted to the delivery target issue
- [ ] `GET /events` returns a live SSE stream of all events
- [ ] `GET /events?issue_id=<id>` filters the stream to events for that issue
- [ ] `ns2 hook list` shows active hooks with their status
- [ ] `ns2 hook disable` / `ns2 hook enable` toggle the enabled flag
- [ ] `ns2 hook delete` removes a hook permanently
- [ ] No panics or unhandled errors in server output
