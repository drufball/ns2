---
targets:
  - crates/harness/src/**/*.rs
  - crates/harness/Cargo.toml
  - crates/anthropic/src/**/*.rs
  - crates/tools/src/**/*.rs
verified: 2026-04-24T12:32:23Z
---


# Agent Harness Spec

## Overview

The harness drives the agent turn loop for a session. It calls the Anthropic Messages API, dispatches tool calls, and feeds results back into the next turn. It runs as an independent tokio task per session and streams events to SSE subscribers as they arrive from Anthropic — no buffering.

Model: `claude-opus-4-7` with extended thinking enabled by default. API reference: https://platform.claude.com/docs/en/api/messages

## Content Blocks

Everything the agent produces is a stream of typed content blocks. The types are:

- **`text`** — prose response from the model, arrives incrementally as a stream of deltas
- **`tool_use`** — the model wants to call a tool; carries `id`, `name`, and `input` (input arrives as streaming JSON, only usable once complete)
- **`tool_result`** — the harness's response to a `tool_use`; carries `tool_use_id`, result content, and an `is_error` flag
- **`thinking`** — the model's internal reasoning (when extended thinking is enabled); streamed like text but displayed separately

A single turn can produce multiple content blocks in sequence — e.g. a `thinking` block followed by a `text` block followed by one or more `tool_use` blocks.

## Turn Loop

Each turn follows this sequence:

1. Build the context (see Context Window Construction), with any queued user messages appended as the final user message
2. Call `POST /messages` with `stream: true` and forward each SSE event to session subscribers immediately as it arrives
3. Assemble the streamed response into complete content blocks
4. On completion, check `stop_reason`:
   - `end_turn` — session is done, mark complete
   - `tool_use` — dispatch all tool calls concurrently, collect results, append both the assistant message and the tool results as a new user message, then loop back to step 1
   - `max_tokens` / `stop_sequence` — treat as completion
   - `content_filter` — unrecoverable, mark session failed
5. Persist the completed turn to SQLite before looping

## Context Window Construction

`claude-opus-4-7` has a 1M token context window. We target 25% of that (~250k tokens) for the combined system prompt and messages array.

### System prompt
- Loaded from `.ns2/agents/<type>.md` at session start. 
- Each file has frontmatter with `name` and `description`; `name` is the agent type. 
- Before use, the file is preprocessed: 
  - frontmatter is stripped
  - any line beginning with `! ` is executed as a bash command and replaced with its stdout. 
- The resulting content becomes the system prompt and counts against the 250k budget.

### Message history
- Sliding window built from the tail of the session's turn history. 
- Each turn's token count is persisted to SQLite when the turn completes (from the `usage` field in `message_delta`), so window construction is a cheap sum over recent turns — no re-counting needed. 
- Walk backward from the most recent turn, accumulating token counts until we'd exceed the budget. The turn that tips it over is still included (the window can run slightly over 25%)
- Never split a turn. A "turn" for this purpose is an atomic unit: an assistant message plus its corresponding tool results, if any.

### User Interrupts

Users (or the orchestrator) can send messages to a running session at any time. These are held in-memory per session and appended to the input of the next turn — the current turn always completes first. The queue is not persisted; if the server stops, queued messages are lost.

## Tool System

Tools are Rust structs implementing a `Tool` trait: name, description, JSON schema, and an async `execute` method. They're registered at harness construction. When a turn ends with `stop_reason: tool_use`, all tool calls are dispatched concurrently. A tool execution error is returned as a `tool_result` with `is_error: true` — the model decides how to proceed; the session is not aborted.

### Standard Tools

**`bash`** — runs a shell command and returns stdout + stderr. Inputs: `command` (string), optional `timeout_ms`. Commands run in the session's working directory.

**`read`** — reads a file and returns its contents. Inputs: `path` (string), optional `start_line` and `end_line` for partial reads. Returns an error if the file does not exist.

**`write`** — writes content to a file, creating it (and any missing parent directories) if needed. Inputs: `path` (string), `content` (string). Overwrites existing files without confirmation — the agent is expected to read before writing if it cares about existing content.

**`edit`** — makes a precise string replacement within a file. Inputs: `path` (string), `old_str` (string), `new_str` (string). Fails if `old_str` is not found or appears more than once in the file.

## Error Handling

If the API call or stream fails mid-turn, the session is marked `failed` and the last successfully persisted turn index is recorded. On retry, the loop resumes from that point.