# Flow 06: Read Tool

Claude reads a file on disk using the `read` tool during an agent run.

## Setup

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
```

Place a `.env` file in the repo root (next to `Cargo.toml`) with your key:
```
ANTHROPIC_API_KEY=sk-ant-...
```

Create a test file for Claude to read:
```bash
echo "The secret value is: ns2-read-tool-test-42" > /tmp/ns2-read-tool-test.txt
```

Start the server:
```bash
$NS2 server start &
```

## Steps

### Create a session asking Claude to read the file

```bash
SESSION_ID=$($NS2 session new --message "Please read the file at /tmp/ns2-read-tool-test.txt and tell me what it says.")
echo "Session: $SESSION_ID"
```

Expected: a UUID printed and stored in `SESSION_ID`.

### Tail the session

```bash
$NS2 session tail --id "$SESSION_ID"
```

The command streams events as Claude responds. Claude will invoke the `read` tool, receive the file contents, and then summarize them.

### Expected output

The tail output should include lines similar to:

```
[turn <uuid>]
[turn <uuid>]
[turn <uuid>]
The file at /tmp/ns2-read-tool-test.txt says: "The secret value is: ns2-read-tool-test-42"
[done]
```

The exact phrasing varies, but Claude's final response must include the string `ns2-read-tool-test-42` — the actual content from the file.

### Verify session status

```bash
$NS2 session list --status completed
```

Expected: the session appears with status `completed`.

### Re-tail to confirm ToolUse and ToolResult blocks are stored

```bash
$NS2 session tail --id "$SESSION_ID"
```

Re-tailing replays stored events. The output should show multiple turns: the user message, the assistant tool call, the tool result, and the final assistant response.

## Acceptance Criteria

- [ ] `ns2 session new --message "..."` creates a session that transitions to `running`
- [ ] Claude invokes the `read` tool with `{"path": "/tmp/ns2-read-tool-test.txt"}`
- [ ] The file contents (`ns2-read-tool-test-42`) appear in Claude's final response
- [ ] The session transitions to `completed`
- [ ] `ns2 session tail` shows ToolUse and ToolResult turns in the event stream
- [ ] No panics or unhandled errors in server output

## Cleanup

```bash
rm -f /tmp/ns2-read-tool-test.txt
bash product-flows/cleanup.sh
```
