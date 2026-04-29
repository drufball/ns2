
# Flow 03: Issue Lifecycle

Create an issue, assign it to an agent, start a session, wait for completion, and mark it done. This is the primary orchestration smoke test.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

```bash
git init /tmp/ns2-smoke && cd /tmp/ns2-smoke
git commit --allow-empty -m "init"
ns2 server start
ns2 agent new --name "swe" --description "Software engineer agent" --body "You are a software engineer. When asked to do something, do it concisely and confirm completion."
```

## Steps

### Step 1: Create an issue with an assignee

```bash
ISSUE=$(ns2 issue new --title "Add a greeting" --body "Create a file called hello.txt with the text Hello World" --assignee swe)
echo "Issue: $ISSUE"
```

Expected: a 4-character issue ID printed to stdout (e.g., `a1b2`).

### Step 2: Verify the issue exists with status open

```bash
ns2 issue list --status open
```

Expected: a table showing the issue with status `open`, assignee `swe`, and auto-generated branch `<id>-add-a-greeting`.

### Step 3: Start the issue

```bash
ns2 issue start --id "$ISSUE"
```

Expected: session UUID printed, issue status transitions to `running`.

### Step 4: Wait for completion

```bash
ns2 issue wait --id "$ISSUE"
```

Expected: blocks until the session completes, then exits 0.

### Step 5: Verify issue status is completed

```bash
ns2 issue list --status completed
```

Expected: the issue shows with status `completed`.

### Step 6: Verify the agent posted a final-turn comment automatically

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

Expected: `OK` — the harness automatically posts the agent's final turn text as a comment with `author == "swe"`.

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

## Acceptance Criteria

- [ ] `ns2 issue new` prints a 4-character issue ID to stdout
- [ ] New issues start with status `open` and an auto-generated branch slug
- [ ] `ns2 issue start` creates a session linked to the issue and sets status to `running`
- [ ] The session uses the issue's assignee as the agent type
- [ ] `ns2 issue wait` blocks until the issue reaches a terminal state and exits 0
- [ ] When the session completes, the agent's final turn text is automatically posted as a comment (`author == assignee`)
- [ ] `ns2 issue complete` adds a manual summary comment
- [ ] `ns2 issue comment` adds comments with the specified author
- [ ] Issue status transitions: open → running → completed
- [ ] No panics or unhandled errors in server output
