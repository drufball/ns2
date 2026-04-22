---
targets:
  - crates/db/src/**/*.rs
  - crates/db/Cargo.toml
  - crates/types/src/**/*.rs
verified: 2026-04-22T19:18:32Z
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
| `agent_type` | TEXT | maps to `.ns2/agents/<type>.md` |
| `status` | TEXT | `created`, `running`, `waiting`, `completed`, `failed`, `cancelled` |
| `branch` | TEXT | git branch the session operates on; always run from repo root |
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
| `index` | INTEGER | block order within turn (explicit from API) |
| `role` | TEXT | `user` or `assistant` |
| `type` | TEXT | discriminator: `text`, `tool_use`, `tool_result`, `thinking` |
| `content` | TEXT | JSON; shape depends on `type` (see below) |

The `content` column is a JSON string whose shape depends on `type`. The `type` column tells the `types` crate which `ContentBlock` enum variant to deserialize `content` into.