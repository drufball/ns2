---
targets:
  - crates/tools/src/**/*.rs
  - crates/harness/src/**/*.rs
severity: warning
verified: 2026-04-25T20:27:55Z
---

# Flow 07: Multi-Tool (Write + Read)

Claude writes a file using the `write` tool and reads it back using the `read` tool during a single agent run.

## Prerequisites

**[Requires: ANTHROPIC_API_KEY]** — loaded from `/tmp/ns2-host.env` mounted into the container.

## Fixture Setup

```bash
docker exec ns2-flow-07 bash /fixtures/init.sh
docker exec ns2-flow-07 bash /fixtures/start-server.sh
```

## Steps

### Create a session asking Claude to write and then read a file

```bash
docker exec ns2-flow-07 bash -c 'cd /repo && ns2 session new --message "Please write the text '\''ns2-multi-tool-test-99'\'' to the file /tmp/ns2-multi-tool-test.txt, then read it back and tell me what it contains." | tee /tmp/session_id.txt && echo "Session created: $(cat /tmp/session_id.txt)"'
```

Expected: a UUID printed alongside `Session created:`.

### Tail the session and wait for completion

```bash
docker exec ns2-flow-07 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

The command streams events as Claude responds. Claude will invoke the `write` tool to create the file, then the `read` tool to read it back, and finally summarize the contents.

Expected output shape:
```
[turn <uuid>]
[tool: write({"path":"/tmp/ns2-multi-tool-test.txt","content":"ns2-multi-tool-test-99"})]
[turn <uuid>]
[result: Wrote N bytes to /tmp/ns2-multi-tool-test.txt]
[turn <uuid>]
[tool: read({"path":"/tmp/ns2-multi-tool-test.txt"})]
[turn <uuid>]
[result: ns2-multi-tool-test-99]
[turn <uuid>]
The file /tmp/ns2-multi-tool-test.txt contains: "ns2-multi-tool-test-99"
[done]
```

The exact phrasing varies, but Claude's final response must include the string `ns2-multi-tool-test-99` — the value that was written by the `write` tool and read back by the `read` tool.

### Verify the file was actually written in the container

```bash
docker exec ns2-flow-07 bash -c 'cat /tmp/ns2-multi-tool-test.txt'
```

Expected output:
```
ns2-multi-tool-test-99
```

### Verify session status

```bash
docker exec ns2-flow-07 bash -c 'cd /repo && ns2 session list --status completed'
```

Expected: the session appears with status `completed`.

### Re-tail to confirm ToolUse and ToolResult blocks are stored

```bash
docker exec ns2-flow-07 bash -c 'cd /repo && ns2 session tail --id "$(cat /tmp/session_id.txt)"'
```

Re-tailing replays stored events. The output should show multiple turns: the user message, the assistant `write` tool call, the write tool result, the assistant `read` tool call, the read tool result, and the final assistant response.

## Acceptance Criteria

- [ ] `ns2 session new --message "..."` creates a session that transitions to `running`
- [ ] Claude invokes the `write` tool with `{"path": "/tmp/ns2-multi-tool-test.txt", "content": "ns2-multi-tool-test-99"}`
- [ ] Claude invokes the `read` tool with `{"path": "/tmp/ns2-multi-tool-test.txt"}`
- [ ] The file `/tmp/ns2-multi-tool-test.txt` exists in the container and contains `ns2-multi-tool-test-99`
- [ ] The string `ns2-multi-tool-test-99` appears in Claude's final response
- [ ] The session transitions to `completed`
- [ ] `ns2 session tail` output includes `[tool: write(...)]`, `[result: ...]`, `[tool: read(...)]`, and `[result: ...]` lines for both tool calls
- [ ] No panics or unhandled errors in server output