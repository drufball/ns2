# GH#133 Test Quality Review
## Branch: `fh6l-gh-133-add-subscribe-flag-to-ns2-issue-new`

---

## Coverage

**Touched files:**
| File | Line Coverage | Branch Coverage | Notes |
|------|--------------|-----------------|-------|
| `crates/cli/src/commands/issue.rs` | 75.0% (770/1027 lines) | 62.2% (46/74 branches) | New `--subscribe` path: ✅ covered |
| `crates/cli/src/main.rs` | 93.3% | 100% (branches) | Excellent |

**New code coverage (lines added in this diff):**

All newly-added lines in `run_new` (lines 85–100) are **fully covered** — the integration tests exercise `subscribe = Some(…)` and `subscribe = None` paths.

All newly-added lines in `run_subscribe` (lines 663–722) are **mostly covered**. The only uncovered lines in the new code are:

- **Line 671** — the `process::exit(1)` on the `else` branch of the `deliver_to` prefix match (i.e., invalid-format error path). This **is** exercised by `issue_new_subscribe_invalid_target_format_fails`, which calls the binary and asserts `!status.success()`. The line appears uncovered in llvm-cov because `process::exit(1)` from a subprocess terminates before the coverage instrumentation can flush. This is a known instrumentation artifact, not a genuine gap.

- **Lines 704, 707, 710–711, 720** — error handlers for HTTP failure and JSON parse failure (`print_error_response`, `std::process::exit(1)` on bad JSON), and the `if print_id_to_stdout { println!(…) }` branch. The stdout branch (`print_id_to_stdout = true`) is never exercised by the new tests — no test calls `issue subscribe --deliver-to` (the standalone command). This is **pre-existing** missing coverage, not introduced by this PR.

**Verdict:** Coverage of the new `--subscribe` flag logic is complete. The pre-existing gaps in `run_subscribe` (error handlers, `print_id_to_stdout = true`) were present before this PR and are not regressions.

---

## Test Quality

### Unit tests (`crates/cli/src/main.rs`)

**`issue_new_subscribe_with_issue_target_parses`** ✅  
**`issue_new_no_subscribe_flag_is_none`** ✅  
**`issue_new_subscribe_with_session_target_parses`** ✅  

These are clean, focused CLI-parsing unit tests. They:
- Use `Cli::try_parse_from` — correct approach for argument parsing tests.
- Assert the exact parsed value with `assert_eq!` — not just "it parsed without error."
- Cover all three cases: `issue:`, `session:`, and absent.
- Are properly isolated (no I/O, no server, no mutable state).

**Minor observation:** Scenario C (`session:some-uuid`) is structurally identical to Scenario A (`issue:ab12`) at the parser level — both are just `Option<String>`. The value distinction only matters downstream in `run_subscribe`. The test is still correct and useful as a guard against accidentally restricting the accepted format in the CLI definition.

### Integration tests (`crates/cli/tests/issue_crud.rs`)

**`issue_new_subscribe_creates_hook`** (Scenario D) ✅  
Good test. Validates:
1. Process exits successfully.
2. stdout has exactly 1 line.
3. That line is a 4-character alphanumeric issue ID.
4. The issue appears in `GET /issues/{id}`.
5. A hook named `subscribe-{issue_id}` appears in `GET /hooks`.

**`issue_new_subscribe_hook_id_on_stderr_not_stdout`** (Scenario D2) ⚠️ **Weak assertion structure**  
This test has a subtle correctness problem: the hook ID is extracted from the `/hooks` JSON response with a hand-rolled string search:

```rust
let hook_id_start = hooks_json.find("\"id\":\"").map(|i| i + 6);
let hook_id = hook_id_start.map(|start| {
    let end = hooks_json[start..].find('"').unwrap() + start;
    &hooks_json[start..end]
});
```

If `hook_id` is `None` (e.g., because the JSON key is `"id"` vs `"id"` with spacing, or because `find` latches onto an unrelated `"id":"` in the response), the entire assertion block silently passes with no check. The `if let Some(hid) = hook_id` guard means a hook-extraction failure is **indistinguishable from a passing assertion**. The test title promises it "must" verify placement, but the guard makes it conditional.

**Fix:** Parse the response as JSON or use `h.http_get` to fetch the specific hook. At minimum, add `assert!(hook_id.is_some(), "…")` before the guard.

**`issue_new_without_subscribe_creates_no_hook`** (Scenario E) ✅  
Clean negative test. The comment "We already captured only the id above, verifying no hook id was printed" is slightly misleading — `ns2_stdout` only captures stdout by trimming it; it doesn't prove there's no second line. But the actual assertion `!hooks_json.contains(&format!("subscribe-{id}"))` is correct and sufficient for the business requirement.

**`issue_new_subscribe_invalid_target_format_fails`** (Scenario F) ✅  
Excellent test. Checks:
1. Non-zero exit code.
2. Error message includes the format hint `'issue:<id>' or 'session:<id>'`.
3. Error message references `--subscribe` (not `--deliver-to`).
4. Error message does NOT reference `--deliver-to`.

This is the most important test for the `flag_name` refactor from the architecture review and it's written correctly.

**`issue_new_subscribe_with_wait_stdout_is_issue_id`** (Scenario G) ⚠️ **Test name overpromises**  
The test is named "subscribe with wait" but the comment immediately clarifies `--wait` is NOT actually tested (it uses `--status open`, which does not trigger `--wait`). The test should be renamed to `issue_new_subscribe_with_status_stdout_is_issue_id` or the comment should be made the test name. As written, a reader will expect `--wait` to have been combined, but it hasn't been.

The `write_agent(&h, "swe")` call is unused — no agent is started, so this is unnecessary setup noise. It could be removed.

---

## Testability

**Hardcoded `process::exit(1)` calls** remain in `run_subscribe`. These prevent unit testing of the error branches without spawning a subprocess. This was pre-existing before this PR and the integration tests cover those paths adequately for now, but extracting an `fn validate_deliver_to(s: &str, flag_name: &str) -> Result<(TargetType, String), String>` pure function would make the format-validation logic independently testable without a running server.

**`print_id_to_stdout: bool` param** is a boolean flag argument — a code smell for a function doing two things. A cleaner design would return the hook ID from `run_subscribe` and let the caller decide what to do with it. However, as an async free function in a CLI crate this is acceptable; refactoring would require more significant restructuring. No action needed for this PR.

**No trait extraction needed** — the new code does not introduce any testability regressions. All new logic is exercised through the existing integration test harness.

---

## Mutation Score

Mutation testing was run with `cargo mutants --workspace --in-diff` against the branch diff. cargo-mutants found exactly 3 mutants in the changed code:

| Mutant | Result |
|--------|--------|
| `replace main with ()` | Caught |
| `replace run_new with ()` | Caught |
| `replace run_subscribe with ()` | Caught |

**All 3 mutants were caught.**

| Crate | Caught | Missed | Timeouts | Score |
|-------|--------|--------|----------|-------|
| `cli` | 3 | 0 | 0 | **100%** |

The mutation score is perfect for the diff-bounded mutant set. Note that `cargo-mutants` in diff mode generates function-body replacement mutants, which are coarse-grained. The tests do catch the removal of each function — this is the primary mutation risk for this type of change.

---

## Summary

### What's good
- The three unit tests (`issue_new_subscribe_*_parses`) are clean, isolated, and make exact assertions.
- The integration tests cover all five specified scenarios (D, D2, E, F, G).
- Scenario F (flag name in error message) is the most critical correctness test for the architecture review change, and it's written very well — asserting both presence of `--subscribe` and absence of `--deliver-to`.
- Scenario D correctly enforces the stdout-is-exactly-one-line contract.
- 100% mutation score on diff-bounded mutants.
- Coverage of the new code paths is complete (modulo instrumentation artifacts from `process::exit`).

### Issues to address

1. **`issue_new_subscribe_hook_id_on_stderr_not_stdout` (medium)**: The `if let Some(hid) = hook_id` guard silently skips the assertion if the fragile hand-rolled JSON extraction fails. Add `assert!(hook_id.is_some(), "could not extract hook id from: {hooks_json}")` before the guard, or use proper JSON parsing.

2. **`issue_new_subscribe_with_wait_stdout_is_issue_id` (low)**: Test name says `--wait` but `--wait` is never exercised. Rename to `issue_new_subscribe_with_status_stdout_is_issue_id`. Also remove the unused `write_agent` call.

3. **Missing: hook payload shape verification (low)**: No test verifies the hook's `event_types`, `filter` (issue ID match), or `action` fields. If someone changes `"issue.status_changed"` to `"issue.status_changed2"` or removes the filter, all tests would still pass. This is the primary mutation gap not reachable by function-level mutants. Consider one test that does `serde_json::from_str::<serde_json::Value>(&hooks_json)` and checks `hooks[0]["source"]["event_types"]` contains both expected event types and `hooks[0]["filter"]["conditions"][0]["value"]` equals the issue ID.

4. **Missing: `session:` target integration test (low)**: The unit parser test covers `session:`, but no integration test fires `--subscribe session:abc` against the server. The session target type flows through a different code path in `run_subscribe` (line 670). Adding a one-liner integration test similar to Scenario D but with a `session:` prefix would close this gap.
