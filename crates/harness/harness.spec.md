# harness crate — module specification

The harness crate runs agent sessions: it owns the turn loop, tool dispatch, hook
execution, system prompt construction, and context window management.

## Module ownership

### `lib.rs` — public interface
Re-exports the public API: `run`, `resolve_session_cwd`, `StubClient`, `HarnessConfig`,
`Error`, and `Result`. Contains no logic of its own. Also hosts the test module, which
imports from all other modules via `use super::*`.

### `loop_.rs` — agent turn loop
Owns the message loop (`run`), tool dispatch (`run_tool_dispatch_loop`), retry logic
(`complete_with_retry`), history loading (`load_history`), message persistence
(`persist_user_message`), and session worktree resolution (`resolve_session_cwd`,
`resolve_session_cwd_with_root`). Orchestrates the other modules: calls into `prompt`,
`hooks`, and the Anthropic client.

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
  └── loop_.rs  →  prompt.rs   (no deps on loop_/hooks)
              →  hooks.rs    (no deps on loop_/prompt)
              →  anthropic client
              →  db
```

`prompt.rs` and `hooks.rs` are leaves: they depend only on external crates
(`agents`, `regex`, `serde_json`, etc.) and not on each other or on `loop_`.

## Invariants

- `hooks.rs` is the single owner of all hook execution. No other module spawns hook
  processes or interprets hook exit codes.
- `prompt.rs` is the single owner of system prompt construction. No other module
  assembles or formats the system prompt string.
- `lib.rs` contains no business logic — only type definitions, re-exports, and tests.
