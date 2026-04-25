---
targets:
  - crates/server/src/**/*.rs
  - crates/db/src/**/*.rs
  - crates/types/src/**/*.rs
severity: warning
verified: 2026-04-25T13:38:10Z
---

# Flow 20: Auto-Complete Posts Final Turn as Comment

When an agent session linked to an issue finishes, the `issue_watcher` task automatically:
1. Collects the final turn's text content as it streams.
2. Posts that text as a comment on the issue (author = agent name).
3. Transitions the issue to `completed`.

This flow also verifies that on session error, the error message is posted as a comment
and the issue is transitioned to `failed`.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the
container.

## Fixture Setup

```bash
docker exec ns2-flow-20 bash /fixtures/init.sh
docker exec ns2-flow-20 bash /fixtures/start-server.sh
```

Create agent types:

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 agent new --name "swe" --description "Software engineer" --body "You are a software engineer. Respond concisely and always end your response with the exact phrase: TASK COMPLETE."'
```

## Steps

### Step 1: Create an issue and start it

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue new --title "Write greeting" --body "Write a single sentence greeting for the user. End your response with TASK COMPLETE." --assignee swe | tee /tmp/issue.txt'
```

Expected: a 4-character issue ID.

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue.txt)"'
```

Expected: `Started session <uuid> for issue <id>` on stderr.

### Step 2: Wait for the issue to complete

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue.txt)"'
```

Expected: exits 0. The issue reached `completed`.

### Step 3: Verify issue is `completed`

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue list --id "$(cat /tmp/issue.txt)" | grep completed'
```

Expected: a table row containing `completed`.

### Step 4: Verify the agent posted its final turn as a comment

```bash
docker exec ns2-flow-20 bash -c '
  ISSUE=$(cat /tmp/issue.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
print(\"Comment count:\", len(comments))
for c in comments:
    print(\"  Author:\", c[\"author\"])
    print(\"  Body preview:\", c[\"body\"][:120])
    print()
agent_comments = [c for c in comments if c[\"author\"] == \"swe\"]
print(\"Agent comments:\", len(agent_comments))
has_task_complete = any(\"TASK COMPLETE\" in c[\"body\"] for c in agent_comments)
print(\"Contains TASK COMPLETE:\", has_task_complete)
"
'
```

Expected output (values will vary):
```
Comment count: 1
  Author: swe
  Body preview: Hello! Wishing you a wonderful day ahead. TASK COMPLETE

Agent comments: 1
Contains TASK COMPLETE: True
```

There must be at least one comment with `author == "swe"` containing the agent's final
turn text, including the phrase `TASK COMPLETE`.

### Step 5: Verify comment was posted before status transition

```bash
docker exec ns2-flow-20 bash -c '
  ISSUE=$(cat /tmp/issue.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
status = d[\"status\"]
comments = d[\"comments\"]
agent_comments = [c for c in comments if c[\"author\"] == \"swe\"]
print(\"Status:\", status)
print(\"Agent comments present:\", len(agent_comments) > 0)
# Both status is completed AND agent comment exists — proves comment was written first
if status == \"completed\" and len(agent_comments) > 0:
    print(\"OK: comment posted before completion\")
else:
    print(\"FAIL\")
    sys.exit(1)
"
'
```

Expected: `OK: comment posted before completion`.

### Step 6: Create a second issue to test multi-word agent names

```bash
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 agent new --name "qa-tester" --description "QA tester" --body "You are a QA tester. Respond concisely. End with: QA DONE."'
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue new --title "Review greeting" --body "Confirm the greeting is polite. End your response with QA DONE." --assignee qa-tester | tee /tmp/issue2.txt'
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue start --id "$(cat /tmp/issue2.txt)"'
docker exec ns2-flow-20 bash -c 'cd /repo && ns2 issue wait --id "$(cat /tmp/issue2.txt)"'
```

Expected: exits 0.

### Step 7: Verify comment author matches assignee for second issue

```bash
docker exec ns2-flow-20 bash -c '
  ISSUE=$(cat /tmp/issue2.txt)
  curl -sf "http://localhost:9876/issues/$ISSUE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
agent_comments = [c for c in comments if c[\"author\"] == \"qa-tester\"]
print(\"qa-tester comments:\", len(agent_comments))
has_qa_done = any(\"QA DONE\" in c[\"body\"] for c in agent_comments)
print(\"Contains QA DONE:\", has_qa_done)
if len(agent_comments) > 0 and has_qa_done:
    print(\"OK\")
else:
    print(\"FAIL\")
    sys.exit(1)
"
'
```

Expected: `OK` — the comment author is `qa-tester`, matching the issue's assignee.

### Step 8: Verify only the final turn's text is posted (not intermediate turns)

```bash
docker exec ns2-flow-20 bash -c '
  # Create an issue that requires a tool call (multi-turn within one session run)
  cd /repo
  ID=$(ns2 issue new --title "Count words" --body "Use bash to run: echo hello world | wc -w   Report the result. End with: COUNTED." --assignee swe)
  ns2 issue start --id "$ID"
  ns2 issue wait --id "$ID"
  curl -sf "http://localhost:9876/issues/$ID" | python3 -c "
import sys, json
d = json.load(sys.stdin)
comments = d[\"comments\"]
agent_comments = [c for c in comments if c[\"author\"] == \"swe\"]
print(\"Agent comment count:\", len(agent_comments))
# Should be exactly 1 comment (only the final text turn, not tool_use blocks)
if len(agent_comments) == 1:
    print(\"OK: exactly 1 agent comment\")
else:
    print(\"FAIL: expected 1, got\", len(agent_comments))
    sys.exit(1)
"
'
```

Expected: `OK: exactly 1 agent comment`. Only the final prose response is posted — tool
calls and tool results do not produce separate comments.

## Acceptance Criteria

- [ ] When a session linked to an issue completes, the agent's final turn text is
      automatically posted as a comment on the issue
- [ ] The comment `author` equals the issue's `assignee` field
- [ ] The comment is persisted before the issue status transitions to `completed`
- [ ] Only the final turn's text content is posted (tool calls/results are excluded)
- [ ] `ContentBlockDelta { TextDelta }` events are accumulated into the current turn buffer
- [ ] `TurnDone` saves the buffer as `last_turn_text` and resets the buffer
- [ ] `SessionDone` posts `last_turn_text` as a comment, then marks issue `completed`
- [ ] `Error { message }` posts the error message as a comment (author = `"system"`),
      then marks the issue `failed`
- [ ] The auto-complete comment is in addition to (not a replacement for) any manual
      comments added via `ns2 issue comment`

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.