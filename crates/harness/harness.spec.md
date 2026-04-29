# harness crate — module specification

The harness crate runs agent sessions: it owns the turn loop, tool dispatch, hook
execution, system prompt construction, and context window management.

## Module ownership

### `lib.rs` — public interface
Re-exports the public API: `run`, `resolve_session_cwd`, `StubClient`, `HarnessConfig`,
`Error`, and `Result`. Contains no logic of its own. Also hosts the test module, which
imports from all other modules via `use super::*`.

### `loop_.rs` — agent turn loop
Owns the message loop (`run`) and tool dispatch (`run_tool_dispatch_loop`). Orchestrates
the other modules: calls into `cwd`, `history`, `retry`, `prompt`, and `hooks`.

### `retry.rs` — 429 retry logic
Owns all Anthropic rate-limit retry behaviour: `max_retries`, `is_rate_limit`, and
`complete_with_retry`. Pure function over the Anthropic client — no DB access, no
knowledge of session or turn state.

### `history.rs` — DB persistence helpers
Owns conversation history persistence: `load_history` and `persist_user_message`.
Reads and writes turns and content blocks; emits the corresponding `SessionEvent`s.
No knowledge of the Anthropic client or retry logic.

### `cwd.rs` — working directory resolution
Owns session working-directory resolution: `resolve_session_cwd` and
`resolve_session_cwd_with_root`. Looks up the session's associated issue branch and
ensures the corresponding git worktree exists. Pure lookup — no side effects beyond
worktree creation.

### `hooks.rs` — hook execution
Single owner of all hook execution logic. Provides `run_hook`, `matching_hook_entries`,
`run_pre_tool_use_hooks`, `run_post_tool_use_hooks`, and `run_stop_hooks`. Has no
knowledge of turn state, context window, or system prompt construction. Pure hook
execution: takes `AgentHooks` and context, runs shell commands, returns results.

### `prompt.rs` — system prompt assembly
Single owner of system prompt construction. Provides `build_preamble` and
`build_system_prompt`. Pure assembly: takes an `AgentDef` and config paths, returns a
`String`. No side effects, no knowledge of hooks or loop state.

### `context.rs` — (reserved)
Token counting and context window management will live here when introduced. Will own
the message list and window budget. No knowledge of hooks or prompts.

## Dependency order

```
lib.rs
  └── loop_.rs  →  cwd.rs      (no deps on loop_/hooks/history/retry)
              →  history.rs  (no deps on loop_/hooks/cwd/retry)
              →  retry.rs    (no deps on loop_/hooks/cwd/history)
              →  prompt.rs   (no deps on loop_/hooks)
              →  hooks.rs    (no deps on loop_/prompt)
              →  anthropic client
              →  db
```

`prompt.rs`, `hooks.rs`, `retry.rs`, `history.rs`, and `cwd.rs` are leaves: they depend
only on external crates (`agents`, `db`, `workspace`, `anthropic`, etc.) and not on each
other or on `loop_`.

## Invariants

- `hooks.rs` is the single owner of all hook execution. No other module spawns hook
  processes or interprets hook exit codes.
- `prompt.rs` is the single owner of system prompt construction. No other module
  assembles or formats the system prompt string.
- `retry.rs` is the single owner of rate-limit retry logic. No other module implements
  backoff or inspects 429 status codes.
- `history.rs` is the single owner of turn/content-block DB persistence within the
  harness. No other module calls `create_turn` or `create_content_block` for history
  purposes.
- `cwd.rs` is the single owner of session working-directory resolution. No other module
  calls `ensure_worktree` on behalf of a session.
- `lib.rs` contains no business logic — only type definitions, re-exports, and tests.
