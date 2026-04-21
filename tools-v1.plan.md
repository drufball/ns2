# tools-v1 Plan

Three vertical slices, each delivering observable end-to-end behaviour. Dependencies flow 1 → 2 → 3.

---

## Slice 1: `read` tool, end-to-end

**Goal:** Claude can read a file on disk during an agent run. A user can ask "what's in foo.txt?" and get a real answer.

**Scope:**
- New `tools` crate with a `Tool` trait and a single `read` implementation
- `anthropic` client updated to send tool definitions in requests and parse `tool_use` / `tool_result` content blocks from streaming responses (including assembling `input_json` deltas)
- Harness gains a minimal tool dispatch loop: when `stop_reason == "tool_use"`, invoke the tool, store the result, feed it back, continue — repeating until `stop_reason == "end_turn"`
- Standard `read` tool registered when a harness session starts
- Harness `Cargo.toml` gains a dependency on `tools`

**Acceptance criteria:**
- `cargo test -p tools` passes with unit tests for `read` (happy path + file-not-found error)
- `cargo test -p anthropic` passes with tests: tool definitions serialise to the correct wire format; a streaming response with a `tool_use` block produces `ContentBlock::ToolUse`; a `tool_result` block in message history serialises correctly
- `cargo test -p harness` passes with tests: single tool call resolved and final text response stored; tool invocation error returned as tool result content and loop continues
- All existing tests continue to pass
- **New product flow 06-read-tool.md:** ask Claude to read a specific file in a temp repo; verify the session completes, the file content appears in the response, and `ToolUse` + `ToolResult` events are visible in the tail output

---

## Slice 2: `bash`, `write`, `edit` tools

**Goal:** The full standard tool suite is available. Claude can execute shell commands and modify files.

**Scope:**
- `bash`, `write`, `edit` implementations added to the `tools` crate
- All four tools registered in the harness at session start
- No changes to the dispatch loop or Anthropic client

**Acceptance criteria:**
- `cargo test -p tools` passes with unit tests for each new tool (happy path + key error cases)
- `cargo test -p harness` passes — no regressions
- **New product flow 07-multi-tool.md:** ask Claude to create a file and then read it back; verify both `write` and `read` tool call events appear in the tail output and the session completes correctly

---

## Slice 3: Multi-turn conversations

**Goal:** A user can send a follow-up message to a `completed` session and Claude responds with full prior context.

**Scope:**

- Session lifecycle: `created → running → completed`. Completed is the terminal state for a given run, not for the session. A user can send a new message to a `completed` session; this transitions it back to `running`, the harness processes the new turn with full conversation history, then it returns to `completed`
- Messages are always queued, never rejected. Branch-level concurrency is maintained by holding queued messages until the branch is free:
  - Message sent to the session currently `running` on a branch → queued, delivered at the next available turn within that session
  - Message sent to a `created` or `completed` session while a different session is `running` on the same branch → queued until the running session completes, then delivered to the completed session
- `session send` works on `created`, `running`, and `completed` sessions
- Harness re-enters the turn loop when a new message arrives on a completed session, with all prior turns included in the API request

**Acceptance criteria:**
- `cargo test -p harness` passes with tests: two sequential tool calls in one run both resolved before final response; a second user message is processed with all prior turns in context
- `cargo test -p server` passes: `POST /sessions/:id/messages` queues messages on both `running` and `completed` sessions; branch concurrency is respected
- **New product flow 08-multi-turn.md:** start a session, ask Claude a question that requires a tool, wait for `completed`, send a follow-up that references the first answer, verify the response is coherent and the tail output shows a second agent run
