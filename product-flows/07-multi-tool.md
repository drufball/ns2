# Flow 07: Multi-Tool (Write + Read)

Claude writes a file using the `write` tool and reads it back using the `read` tool during a single agent run.

## Setup

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
```

Place a `.env` file in the repo root (next to `Cargo.toml`) with your key:
```
ANTHROPIC_API_KEY=sk-ant-...
```

Start the server:
```bash
$NS2 server start &
```

## Steps

### Create a session asking Claude to write and then read a file

```bash
SESSION_ID=$($NS2 session new --message "Please write the text 'ns2-multi-tool-test-99' to the file /tmp/ns2-multi-tool-test.txt, then read it back and tell me what it contains.")
echo "Session: $SESSION_ID"
```

Expected: a UUID printed and stored in `SESSION_ID`.

### Tail the session

```bash
$NS2 session tail --id "$SESSION_ID"
```

The command streams events as Claude responds. Claude will invoke the `write` tool to create the file, then the `read` tool to read it back, and finally summarize the contents.

### Expected output

The tail output should include lines similar to:

```
[turn <uuid>]
[turn <uuid>]
[turn <uuid>]
[turn <uuid>]
[turn <uuid>]
The file /tmp/ns2-multi-tool-test.txt contains: "ns2-multi-tool-test-99"
[done]
```

The exact phrasing varies, but Claude's final response must include the string `ns2-multi-tool-test-99` — the value that was written by the `write` tool and read back by the `read` tool.

### Verify session status

```bash
$NS2 session list --status completed
```

Expected: the session appears with status `completed`.

### Re-tail to confirm ToolUse and ToolResult blocks are stored

```bash
$NS2 session tail --id "$SESSION_ID"
```

Re-tailing replays stored events. The output should show multiple turns: the user message, the assistant `write` tool call, the write tool result, the assistant `read` tool call, the read tool result, and the final assistant response.

## Acceptance Criteria

- [ ] `ns2 session new --message "..."` creates a session that transitions to `running`
- [ ] Claude invokes the `write` tool with `{"path": "/tmp/ns2-multi-tool-test.txt", "content": "ns2-multi-tool-test-99"}`
- [ ] Claude invokes the `read` tool with `{"path": "/tmp/ns2-multi-tool-test.txt"}`
- [ ] The file contents (`ns2-multi-tool-test-99`) appear in Claude's final response
- [ ] The session transitions to `completed`
- [ ] `ns2 session tail` shows both `write` and `read` ToolUse and ToolResult turns in the event stream
- [ ] No panics or unhandled errors in server output

## Cleanup

```bash
rm -f /tmp/ns2-multi-tool-test.txt
bash product-flows/cleanup.sh
```
