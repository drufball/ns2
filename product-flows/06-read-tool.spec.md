---
targets:
  - crates/tools/src/**/*.rs
  - crates/harness/src/**/*.rs
severity: warning
verified: 2026-04-25T10:03:20Z
---

# Flow 06: Read Tool

Claude reads a file on disk using the `read` tool during an agent run.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

## Fixture Setup

```bash
docker exec ns2-flow-06 bash /fixtures/init.sh
docker exec ns2-flow-06 bash /fixtures/start-server.sh
docker exec ns2-flow-06 bash /fixtures/seeded-files.sh
```

## Steps

### Create a session asking Claude to read the file

```bash
docker exec ns2-flow-06 bash -c 'cd /repo && ns2 session new --message "Please read the file at /repo/read-test.txt and tell me what it says." | tee /tmp/session_id.txt && echo "Session created: $(cat /tmp/session_id.txt)"'
```

Expected: a UUID printed alongside `Session created:`.

### Tail the session and wait for completion

```bash
docker exec ns2-flow-06 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

The command streams events as Claude responds. Claude will invoke the `read` tool, receive the file contents, and then summarize them.

Expected output shape:
```
[turn <uuid>]
[tool: read({"path":"/repo/read-test.txt"})]
[turn <uuid>]
[result: The secret value is: ns2-read-tool-test-42]
[turn <uuid>]
The file at /repo/read-test.txt says: "The secret value is: ns2-read-tool-test-42"
[done]
```

The exact phrasing varies, but Claude's final response must include the string `ns2-read-tool-test-42` — the actual content from the file.

### Verify session status

```bash
docker exec ns2-flow-06 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the session appears with status `completed`.

### Re-tail to confirm ToolUse and ToolResult blocks are stored

```bash
docker exec ns2-flow-06 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

Re-tailing replays stored events. The output should show multiple turns: the user message, the assistant tool call, the tool result, and the final assistant response.

## Acceptance Criteria

- [ ] `ns2 session new --message "..."` creates a session that transitions to `running`
- [ ] Claude invokes the `read` tool with `{"path": "/repo/read-test.txt"}`
- [ ] The file contents (`ns2-read-tool-test-42`) appear in Claude's final response
- [ ] The session transitions to `completed`
- [ ] `ns2 session tail` output includes `[tool: read({"path": ...})]` and `[result: ...]` lines for the tool call
- [ ] Re-tailing a completed session replays all turns including tool call and result
- [ ] No panics or unhandled errors in server output