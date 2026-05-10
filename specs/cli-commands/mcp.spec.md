---
targets:
  - crates/cli/src/commands/mcp.rs
  - ns2.toml
verified: 2026-05-10T18:53:25Z
---

# ns2 mcp

`ns2 mcp` runs the ns2 MCP server plugin. It is intended to be registered as a Claude Code MCP server entry so that Claude Code receives push-based issue notifications without polling.

## What it does

1. Reads `[server] url` from `ns2.toml` (defaults to `http://127.0.0.1:9876`).
2. Reads `channel-id` from `ns2.local.toml` (required; error if missing).
3. Completes the JSON-RPC MCP `initialize` handshake on stdin/stdout, advertising the `experimental.claude/channel` capability.
4. Opens an SSE connection to `GET /events?event_type=mcp.channel_notification&channel_id=<id>`, filtering for `McpChannelNotification` events addressed to this developer's channel.
5. Forwards each notification to stdout as a JSON-RPC `notifications/claude/channel` message.

**Sequencing:** The MCP `initialize` handshake MUST complete before the SSE connection is opened. Claude Code sends `initialize` first and waits for a response — the SSE connection is only opened after the handshake succeeds.

**Failure handling:** If the SSE connection fails (server not running), a warning is written to stderr and the process continues so Claude Code's session is not interrupted.

## Configuration files

| File | Key | Description |
|------|-----|-------------|
| `ns2.toml` | `[server] url` | URL of the running ns2 server. Defaults to `http://127.0.0.1:9876`. |
| `ns2.local.toml` | `channel-id` | Per-developer channel identifier. **Not committed to git.** |

`ns2.local.toml` example:
```toml
channel-id = "alice"
```

`ns2.toml` example:
```toml
[server]
url = "http://127.0.0.1:9876"
```

## Typical setup

Register in `.claude/mcp.json` (or equivalent Claude Code config):
```json
{
  "mcpServers": {
    "ns2": {
      "command": "ns2",
      "args": ["mcp"]
    }
  }
}
```

Subscribe an issue to your channel so you receive notifications:
```bash
ns2 issue subscribe --id <issue-id> --deliver-to mcp:alice
```

When the issue changes status, Claude Code receives a `notifications/claude/channel` notification containing the rendered body and a `meta` map with fields like `issue_id`, `from`, and `to`.

## How it connects to the rest of the system

1. A hook with `action.type = "mcp_notify"` and `action.channel_id = "alice"` fires on matching events.
2. The `hooks` crate handler emits `SystemEvent::McpChannelNotification { channel_id: "alice", body, meta }` on the event bus.
3. The server's SSE endpoint streams this event to any client subscribed with `channel_id=alice`.
4. `ns2 mcp` receives the SSE event and writes it to stdout as a JSON-RPC notification.