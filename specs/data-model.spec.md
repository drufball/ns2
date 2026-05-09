---
targets:
  - crates/db/src/**/*.rs
  - crates/db/Cargo.toml
  - crates/types/src/**/*.rs
verified: 2026-05-09T06:29:20Z
---

# Data Model Spec

## Overview

SQLite via sqlx. The `db` crate owns all schema and migrations — nothing outside it writes SQL. Types in this spec map to the shared types in the `types` crate.

## Tables

### `sessions`

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | UUID |
| `name` | TEXT | human-readable label |
| `agent` | TEXT | optional; maps to `.ns2/agents/<name>.md` |
| `status` | TEXT | `created`, `running`, `completed`, `failed`, `cancelled`, `waiting` |
| `created_at` | INTEGER | unix timestamp |
| `updated_at` | INTEGER | unix timestamp |

### `turns`

One row per completed agent turn (assistant message + any tool results).

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | UUID |
| `session_id` | TEXT FK | → `sessions.id` |
| `token_count` | INTEGER | from `usage` in `message_delta`; used for context window construction |
| `created_at` | INTEGER | unix timestamp |

### `content_blocks`

One row per content block within a turn, in order.

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | UUID |
| `turn_id` | TEXT FK | → `turns.id` |
| `block_index` | INTEGER | block order within turn (explicit from API) |
| `role` | TEXT | `user` or `assistant` |
| `content` | TEXT | JSON; self-describing — shape includes a `type` discriminator |
| `created_at` | INTEGER | unix timestamp |

The `content` column is a self-describing JSON blob. There is no separate `type` column — the JSON itself carries a `type` field (`text`, `tool_use`, `tool_result`, `thinking`) that the `types` crate uses to select the right `ContentBlock` enum variant during deserialization.

### `issues`

Work items that can be assigned to agents and tracked through a lifecycle.

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | 4-character random alphanumeric (see below) |
| `title` | TEXT | short description |
| `body` | TEXT | full issue text |
| `status` | TEXT | `open`, `running`, `completed`, `failed`, `cancelled`, `waiting` |
| `branch` | TEXT | git branch the issue operates on |
| `assignee` | TEXT | optional; agent type name |
| `session_id` | TEXT | optional; UUID of the linked agent session |
| `parent_id` | TEXT | optional; ID of the parent issue |
| `blocked_on` | TEXT | JSON array of issue IDs that must complete first |
| `comments` | TEXT | JSON array of `{author, created_at, body}` objects |
| `created_at` | INTEGER | unix timestamp |
| `updated_at` | INTEGER | unix timestamp |

**Issue ID design.** Issue IDs are 4 characters (lowercase alphanumeric, e.g. `x7qm`) rather than UUIDs. The short form is human-readable in CLI output and easy to type. IDs are derived from UUID v4 bytes mapped through a 36-character alphabet, so they are random enough for the expected issue counts (collision probability is negligible at hundreds of issues). The generation logic lives in the `issues` crate.

**`blocked_on` and `comments` as JSON TEXT.** Both fields are stored as JSON strings in TEXT columns rather than join tables. The access pattern for both is always "read/write the whole list at once" — there are no queries that filter by individual blocked-on IDs or comment authors. A join table would add schema complexity (cascade deletes, extra migrations, multi-row inserts) with no query benefit. SQLite's JSON support is available if needed in the future.

### `hooks`

Event-driven hooks that fire when a `SystemEvent` matches their filter.

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | 4-character random alphanumeric |
| `name` | TEXT | human-readable label |
| `source_type` | TEXT | `internal`, `external`, or `timer` |
| `source` | TEXT | JSON; shape varies by `source_type` |
| `filter` | TEXT | optional JSON; field conditions that must match for the hook to fire |
| `action_type` | TEXT | `send_message`, `create_issue`, or `run_shell` |
| `action` | TEXT | JSON; shape varies by `action_type` |
| `enabled` | INTEGER | `1` = active, `0` = disabled |
| `created_by` | TEXT | optional; who created the hook |
| `created_at` | TEXT | ISO-8601 timestamp |
| `updated_at` | TEXT | ISO-8601 timestamp |

### `hook_executions`

One row per hook firing attempt, recording the lifecycle from trigger to outcome.

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | UUID |
| `hook_id` | TEXT FK | → `hooks.id` |
| `triggered_at` | TEXT | ISO-8601 timestamp when the event was received |
| `event_payload` | TEXT | JSON snapshot of the `SystemEvent` that triggered the hook |
| `status` | TEXT | `running`, `completed`, or `failed` |
| `result` | TEXT | optional; success message or error string |
| `completed_at` | TEXT | optional; ISO-8601 timestamp when the action finished |

## Types (types crate)

The `types` crate mirrors the DB schema in Rust:

- `Session`, `SessionStatus` — maps to the `sessions` table
- `Turn` — maps to the `turns` table
- `ContentBlock`, `Role` — maps to `content_blocks`
- `Issue`, `IssueStatus`, `IssueComment` — maps to the `issues` table
- `Hook`, `HookSource`, `HookAction`, `HookFilter` — maps to the `hooks` table
- `HookExecution`, `ExecutionStatus` — maps to the `hook_executions` table

`HookSource` has three variants:
- `Internal { event_types: Vec<String> }` — fires when a matching `SystemEvent` is published on the event bus
- `External { secret: Option<String> }` — fires via `POST /hooks/:id/trigger`
- `Timer { schedule: String }` — fires on a 5-field cron schedule (e.g. `"0 9 * * 1"` = Monday 9 am UTC)

## Events (events crate)

`SystemEvent` is the top-level envelope on the global event bus:

- `Session { session_id, event: SessionEvent }` — harness turn-level events
- `Issue(IssueEvent)` — issue lifecycle events (created, status changed, comment added)
- `External { hook_id, payload }` — fired when an external webhook is received
- `TimerFired { hook_id, fired_at }` — fired by the timer scheduler for each enabled timer hook whose cron schedule falls within the current tick window