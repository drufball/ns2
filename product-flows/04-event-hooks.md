# Flow 04: Event Hooks & Subscriptions

Create internal hooks that react to issue state changes and deliver notifications without blocking. This is the primary orchestration smoke test for the hook system.

## Setup

```bash
/fixtures/init-git-repo.sh
/fixtures/copy-env.sh
cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &
sleep 3
/fixtures/create-swe-agent.sh
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

Expected: a 4-character hook ID printed to stdout; a hook is created in the system with name `subscribe-<WORK>`.

### Step 3b (optional): Subscribe recursively to an issue tree

```bash
ROOT=$(ns2 issue new --title "Root Task" --body "Parent issue")
CHILD=$(ns2 issue new --title "Child Task" --body "Child issue" --parent "$ROOT")
RHOOK=$(ns2 issue subscribe --id "$ROOT" --deliver-to "issue:$WATCHER" --recursive)
echo "Recursive hook: $RHOOK"
```

Expected: a 4-character hook ID printed to stdout; a hook named `subscribe-<ROOT>-recursive` is created with a `contains` condition on `data.issue.ancestor_ids`.

### Step 4: Verify the hook exists

```bash
ns2 hook list
```

Expected: a table showing the hook with the work issue ID in its name/filter, enabled=true.

### Step 5: Start the work issue and wait for completion

```bash
ns2 issue set-status --id "$WORK" --status in_progress
ns2 issue wait --id "$WORK"
```

Expected: exits 0 when the work issue reaches a terminal state. (`ns2 issue start` no longer exists; use `set-status --status in_progress` to auto-start execution.)

### Step 6: Verify the watcher received a notification comment

```bash
sleep 2  # allow hook delivery to complete

ns2 issue show --id "$WATCHER" --json | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = [c for c in d['comments'] if 'completed' in c['body'].lower() or 'running' in c['body'].lower()]
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
ISSUE2=$(ns2 issue new --title "Test issue events" --body "test" --assignee swe)
timeout 5 curl -sN "http://localhost:9876/events?issue_id=$ISSUE2" &
sleep 1
ns2 issue set-status --id "$ISSUE2" --status in_progress 2>/dev/null || true
sleep 2
```

Expected: SSE events appear for that issue when it starts (status_changed). `--assignee swe` is required for `set-status in_progress` to auto-start execution.

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

### Step 10: MCP subscribe — create a McpNotify hook

```bash
MCP_HOOK=$(ns2 issue subscribe --id "$WORK" --deliver-to "mcp:alice-laptop")
echo "MCP hook: $MCP_HOOK"
```

Expected: a 4-character hook ID printed to stdout; the hook is visible via `ns2 hook list` with action type `mcp_notify`.

### Step 11: MCP channel notification SSE filter

```bash
# After an issue status change, an McpChannelNotification is emitted on the bus.
# Verify it is visible on the filtered SSE endpoint:
timeout 3 curl -sN "http://localhost:9876/events?event_type=mcp.channel_notification&channel_id=alice-laptop" | head -10 || true
```

Expected: SSE data lines appear with `"type":"mcp_channel_notification"` when a subscribed issue changes status.

### Step 12: ns2 mcp — MCP handshake and notification forwarding

```bash
# ns2 mcp performs an MCP initialization handshake on stdin/stdout.
# Verify it advertises the claude/channel experimental capability.
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' \
  | ns2 mcp | head -3
```

Expected: a JSON-RPC response containing `"experimental":{"claude/channel":{}}` in the capabilities.

## Acceptance Criteria

- [ ] `ns2 issue subscribe` creates an internal hook visible via `ns2 hook list`
- [ ] `ns2 issue subscribe --recursive` creates a hook named `subscribe-<id>-recursive` with a `contains` condition on `data.issue.ancestor_ids`
- [ ] When `--recursive` is passed to `issue new --subscribe`, the created hook uses the recursive filter
- [ ] `ancestor_ids` is populated on the `Issue` type and included in all event payloads by walking the parent chain
- [ ] When a subscribed issue changes status, a notification comment is posted to the delivery target issue
- [ ] `GET /events` returns a live SSE stream of all events
- [ ] `GET /events?issue_id=<id>` filters the stream to events for that issue
- [ ] `ns2 hook list` shows active hooks with their status
- [ ] `ns2 hook disable` / `ns2 hook enable` toggle the enabled flag
- [ ] `ns2 hook delete` removes a hook permanently
- [ ] `ns2 issue subscribe --deliver-to mcp:<channel-id>` creates a `McpNotify` hook
- [ ] `McpNotify` hook action emits `SystemEvent::McpChannelNotification` onto the event bus when triggered
- [ ] `GET /events?event_type=mcp.channel_notification&channel_id=<id>` filters the stream to MCP notifications for that channel
- [ ] `ns2 mcp` performs the MCP JSON-RPC initialization handshake (experimental `claude/channel` capability)
- [ ] `ns2 mcp` forwards `McpChannelNotification` events as `notifications/claude/channel` JSON-RPC notifications to stdout
- [ ] `ns2 mcp` reads `channel-id` from `ns2.local.toml`; exits with clear error if missing
- [ ] `ns2.local.toml` is gitignored
- [ ] No panics or unhandled errors in server output
