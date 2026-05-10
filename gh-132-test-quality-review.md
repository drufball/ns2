# GH#132 Test Quality Review

Branch: `4llr-gh-132-redesign-webhook-timer-system-named-events-unified-hook-interface`  
Reviewed by: ns2 test-quality agent  
All tests pass. Total workspace coverage: **89.0% lines / 89.7% branches**.

---

## Coverage

### Files with low / zero coverage from this PR's diff

| File | Line cov | Branch cov | Notes |
|------|----------|------------|-------|
| `crates/server/src/routes/named_event.rs` | **0%** | **0%** | All 4 route handlers uncovered |
| `crates/cli/src/commands/event.rs` | **0%** | **0%** | All 3 CLI dispatch functions uncovered |
| `crates/server/src/routes/hook.rs` | 78% | 94% | Pre-existing gap; unchanged by this PR |
| `crates/server/src/routes/session.rs` | 67% | 95% | Pre-existing gap; unchanged by this PR |

#### `named_event.rs` — 0% coverage (all 75 lines)

The file's `#[cfg(test)]` block contains the comment  
> "Named event route integration tests are in server/src/lib.rs test module."  

But **no such tests exist** in `server/src/lib.rs`. The four route handlers —  
`create_event`, `list_events`, `get_event`, `delete_event` — are called from the  
router but never exercised by the test suite. The validation path for an invalid  
cron schedule (`StatusCode::BAD_REQUEST`) is completely uncovered.

#### `commands/event.rs` — 0% coverage (all 153 lines)

`run_new`, `run_list`, and `run_delete` are the CLI-layer wrappers for  
`POST/GET/DELETE /named-events`. They are dispatched from `main.rs` but  
the CLI parse tests in `main.rs` only exercise clap parsing — they never  
call the dispatch arms that invoke `commands::event::*`. No HTTP-level stub  
tests exist for these functions.

---

## Test Quality

### Strong areas

* **`crates/types/src/lib.rs`** — `EventKind` and `Event` serde round-trips are  
  thorough: both `Webhook` and `Timer` variants are tested, the `enabled=false`  
  case is covered, and the snake_case tag shape is asserted with `serde_json::to_value`.

* **`crates/db/src/lib.rs` (`SqliteEventStore`)** — CRUD is complete: create, get  
  by id, get by name, list, delete, duplicate-name error, not-found errors on  
  get/delete, `enabled=false` round-trip, timer kind, webhook with secret. Uses  
  `#[sqlx::test]` which gives a clean per-test in-memory database.

* **`crates/hooks/src/lib.rs`** — `matches_event` coverage is excellent: every  
  system-event variant has a positive and (where appropriate) negative test. The  
  new `external.*` and `timer.*` event-name matching is explicitly tested with  
  `hook_matches_external_event_by_name`, `hook_does_not_match_wrong_external_event_name`,  
  and `hook_matches_timer_event_by_name`. Wildcard `"*"` and empty `event_names`  
  edge cases are covered.

* **`crates/hooks/src/timer.rs`** — `should_fire` tests cover the 60-second window  
  boundary, invalid schedules, and every-minute cadence. `process_timer_events`  
  tests cover disabled events, webhook events being skipped, multiple simultaneous  
  matches, and correct `event_name`/`fired_at` in the emitted `TimerFired` event.  
  All tests use a `StubEventStore` mock — no live DB.

* **`crates/server/src/lib.rs`** — The new `POST /webhooks/:event_id` route tests  
  are thorough: timer-event returns 404, disabled-event returns 404, no-secret/no-sig  
  returns 200, correct-sig returns 200, missing-sig returns 401, wrong-sig returns  
  401, invalid JSON returns 400, and the end-to-end evaluator dispatch  
  (`test_webhook_evaluator_dispatches_action_for_external_event`) proves the  
  full `External → hook evaluator → comment` pipeline.

### Issues found

#### 1. CLI `event` subcommand has zero parse tests (`crates/cli/src/main.rs`)

Every other added subcommand (`hook new`, `issue subscribe`, etc.) has a  
corresponding set of `fn hook_new_parses_*` / `fn issue_subscribe_parses_*` tests  
in `main.rs`. The `event` subcommand has **none**. There are no tests for:

- `ns2 event new <name> --type webhook`
- `ns2 event new <name> --type timer --schedule "* * * * *"`
- `ns2 event new <name> --type webhook --secret abc123 --description "desc"`
- `ns2 event list`
- `ns2 event delete --id <id>`
- `ns2 event new` with `--type timer` and no `--schedule` (should fail gracefully)

Because the dispatch arms are never exercised, the 0% line-coverage on  
`commands/event.rs` is a direct consequence.

#### 2. `/named-events` route handlers have no integration tests

Despite the comment in `named_event.rs` directing reviewers to `server/src/lib.rs`,  
there are zero `#[tokio::test]` functions in that file exercising the  
`POST /named-events`, `GET /named-events`, `GET /named-events/:id`, or  
`DELETE /named-events/:id` routes. This means:

- The 201 status code on successful create is untested.
- The `EventApiError` not-found (404) path for `get_event` / `delete_event` is untested.
- The cron-schedule validation (`StatusCode::BAD_REQUEST` for invalid schedule) is untested.
- The duplicate-name 500 from the DB UNIQUE constraint propagation is untested.

#### 3. `generate_event_id` is functionally untested

`generate_event_id` in `crates/hooks/src/lib.rs` is identical to `generate_hook_id`  
except for its name. `generate_hook_id` has three tests (`generate_hook_id_is_4_chars`,  
`_is_unique`, `_uses_full_alphabet`). **No parallel tests exist for `generate_event_id`**.

#### 4. `event_kind_type_str` output is never independently verified

`event_kind_type_str` maps `EventKind::Webhook → "webhook"` and `EventKind::Timer → "timer"`.  
The DB round-trip tests confirm the data persists, but no test reads the raw `kind_type`  
column from SQLite to assert the stored string value (unlike `test_action_type_str_stored_correctly_*`  
in `SqliteHookStore`). This means a mutation that returns `""` or swaps the two  
strings would survive without any raw-SQL assertion.

#### 5. Timer schedule validation in `POST /named-events` is untested

`named_event::create_event` validates the cron schedule when `kind = Timer`:

```rust
if let EventKind::Timer { ref schedule } = req.kind {
    if let Err(e) = hooks::cron::next_after(schedule, Utc::now()) {
        return (StatusCode::BAD_REQUEST, ...).into_response();
    }
}
```

No test sends an invalid schedule string and checks for 400.

---

## Testability

### `commands/event.rs` is untestable at the unit level

`run_new`, `run_list`, and `run_delete` each make live `reqwest` HTTP calls and  
`std::process::exit` on error. There is no dependency-injection point for the HTTP  
client, which makes unit-testing the success and error paths impossible without  
spinning up a real server.

**Recommendation:** Extract a thin `EventApiClient` trait (or reuse a generic  
`post_json` helper already present in `client.rs`) and pass it in. Then the  
`run_new` logic can be tested against a mock that returns canned JSON without a  
running server.

The same pattern already works in `execute::run_action` in `hooks/src/lib.rs`,  
which takes `&dyn HookStore` — apply the same approach to the CLI event commands.

### `named_event.rs` has a dead-code error type with no reachable branch

`EventApiError::into_response` maps `StoreError::NotFound` to 404 and everything  
else to 500. The `Sqlx` and `Migrate` variants of `StoreError` can never be returned  
from `EventStore` operations (they are construction-time errors), so the 500 branch is  
unreachable in practice. Either remove it or add a comment explaining why it is a  
defensive fallback.

---

## Mutation Score

### Run details (with `--lib --tests`, excluding integration tests that require a live git environment)

58 mutants tested: **35 caught / 10 missed / 13 unviable**

| Crate | Caught | Missed | Timeouts | Score |
|-------|--------|--------|----------|-------|
| `cli` (`commands/event.rs`, `commands/hook.rs`) | 3 | 6 | 0 | 33% |
| `db` (`lib.rs` — EventStore + HookStore) | 9 | 2 | 0 | 82% |
| `hooks` (`lib.rs` + `timer.rs`) | 15 | 4 | 0 | 79% |
| `server` (`lib.rs` + `routes/named_event.rs`) | 8 | 2 | 0 | 80% |
| **Total** | **35** | **14** | **0** | **71%** |

> Note: `13 unviable` mutants were type-system rejections (changing return types  
> that don't compile); these are not meaningful test gaps.

---

### Missed mutants with real-world consequence

#### 1. `crates/cli/src/commands/event.rs:13:5` — `replace run_new with ()`

**What it means:** The entire `event new` command body becomes a no-op. The event  
is never POSTed, nothing is printed.  
**Real-world consequence:** `ns2 event new ci-complete --type webhook` silently does  
nothing and exits successfully.  
**Test to catch it:** A CLI parse-dispatch test that calls `run_new` against a mock  
HTTP server and asserts a POST to `/named-events` was made with the correct body.

#### 2. `crates/cli/src/commands/event.rs:50:8` — `delete ! in run_new`

**What it means:** The `!resp.status().is_success()` guard becomes `resp.status().is_success()`.  
On a **successful** response the CLI now calls `print_error_response` and exits 1.  
**Real-world consequence:** `ns2 event new` fails with an error message on every  
successful creation.  
**Test to catch it:** Mock a 201 response; assert the command exits 0 and prints  
the event ID to stdout.

#### 3. `crates/cli/src/commands/event.rs:111:22` — `replace == with != in run_delete`

**What it means:** `if resp.status() == reqwest::StatusCode::NOT_FOUND` becomes  
`!= NOT_FOUND`. On a 404 the CLI no longer prints a user-friendly "event not found"  
message; on a 204 it does print it and exits 1.  
**Real-world consequence:** Deleting an existing event always fails with a misleading  
"event not found" error; deleting a missing event silently succeeds.  
**Test to catch it:**  
- Assert that deleting a missing ID prints "event not found" and exits non-zero.  
- Assert that deleting a real ID exits 0.

#### 4. `crates/db/src/lib.rs:796:5` — `replace event_kind_type_str -> &'static str with ""`

**What it means:** All events are stored with an empty `kind_type` in SQLite  
(the `kind_type` column is a denormalized discriminant column, separate from the  
JSON `kind` column which still carries correct data).  
**Real-world consequence:** If a future query filters `kind_type` (e.g. "list only  
timer events"), it would return an empty set. The current `parse_event_row` does  
not read `kind_type` — it reads the full `kind` JSON — so the column silently  
corrupts without any test observing it.  
**Test to catch it:**  
```rust
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_event_kind_type_column_is_stored_correctly(pool: SqlitePool) {
    use sqlx::Row;
    let store = SqliteEventStore::new(pool.clone());
    store.create_event(&make_event("e1", "ci")).await.unwrap();
    let row = sqlx::query("SELECT kind_type FROM events WHERE id = 'e1'")
        .fetch_one(&pool).await.unwrap();
    let kind_type: String = row.get("kind_type");
    assert_eq!(kind_type, "webhook");
}
```

#### 5. `crates/hooks/src/lib.rs:252:5` — `replace generate_event_id -> String with String::new()`

**What it means:** `generate_event_id()` always returns `""`. Every event created  
via the CLI or server gets an empty string as its ID.  
**Real-world consequence:** All events share the same (empty) ID; the second  
`POST /named-events` would collide in the DB UNIQUE constraint on `id`.  
**Test to catch it:**  
```rust
#[test]
fn generate_event_id_is_4_chars_lowercase_alphanumeric() {
    let id = generate_event_id();
    assert_eq!(id.len(), 4);
    assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
}
#[test]
fn generate_event_id_is_unique() {
    let ids: std::collections::HashSet<_> = (0..100).map(|_| generate_event_id()).collect();
    assert!(ids.len() > 90);
}
```

#### 6. `crates/hooks/src/timer.rs:79:5` — `replace spawn_timer_scheduler with ()`

**What it means:** The timer scheduler is never spawned.  
**Real-world consequence:** All `Timer`-kind events are permanently silent; no  
`TimerFired` events are ever emitted, no timer hooks fire.  
**Test to catch it:** An integration test in `server/src/lib.rs` that:  
1. Creates a `Timer` event with schedule `"* * * * *"`.  
2. Subscribes to the event bus.  
3. Manually calls `process_timer_events` (the function is `pub(crate)`) or waits  
   up to a reasonable bound.  
4. Asserts a `SystemEvent::TimerFired` was received.  
The `spawn_timer_scheduler` function itself is essentially untestable in a unit  
test without timing, but `process_timer_events` already has tests — the gap is  
that nothing tests the wiring from `run()` through `spawn_timer_scheduler` to an  
actual emission.

#### 7. `crates/server/src/routes/named_event.rs:31:9` — `replace into_response with Default::default()`

**What it means:** When a named-event route returns an error (e.g. 404 or 400),  
the response body and status are replaced by the empty default (200 with empty body).  
**Real-world consequence:** `GET /named-events/no-such-id` returns 200 instead of  
404; `POST /named-events` with an invalid cron schedule returns 200 instead of 400.  
**Test to catch it:** The missing integration tests for `/named-events` (see §2 above).

---

## Summary of recommended new tests

| Priority | File | Test name | What to assert |
|----------|------|-----------|----------------|
| 🔴 High | `server/src/lib.rs` | `test_create_named_event_returns_201` | POST /named-events → 201, id is 4-char |
| 🔴 High | `server/src/lib.rs` | `test_list_named_events_returns_created` | GET /named-events → contains event |
| 🔴 High | `server/src/lib.rs` | `test_get_named_event_by_id` | GET /named-events/:id → 200 + correct fields |
| 🔴 High | `server/src/lib.rs` | `test_get_named_event_not_found` | GET /named-events/xxxx → 404 |
| 🔴 High | `server/src/lib.rs` | `test_delete_named_event` | DELETE /named-events/:id → 204, then 404 |
| 🔴 High | `server/src/lib.rs` | `test_create_named_event_invalid_cron_returns_400` | POST with bad schedule → 400 |
| 🔴 High | `cli/src/main.rs` | `event_new_parses_webhook_type` | `ns2 event new ci --type webhook` parses correctly |
| 🔴 High | `cli/src/main.rs` | `event_new_parses_timer_type_with_schedule` | `ns2 event new hb --type timer --schedule "* * * * *"` |
| 🔴 High | `cli/src/main.rs` | `event_list_parses` | `ns2 event list` parses |
| 🔴 High | `cli/src/main.rs` | `event_delete_parses_id` | `ns2 event delete --id abc1` parses |
| 🟡 Medium | `hooks/src/lib.rs` | `generate_event_id_is_4_chars` | length=4, alphanumeric |
| 🟡 Medium | `hooks/src/lib.rs` | `generate_event_id_is_unique` | 100 calls → >90 distinct |
| 🟡 Medium | `db/src/lib.rs` | `test_event_kind_type_column_is_stored_correctly` | raw SQL assert `kind_type = "webhook"` and `"timer"` |
| 🟢 Low | `server/src/lib.rs` | `test_create_named_event_duplicate_name_returns_error` | second POST same name → non-2xx |
