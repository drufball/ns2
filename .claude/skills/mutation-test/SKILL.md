---
name: mutation-test
description: Run mutation testing with cargo-mutants on the Rust workspace. Use when asked to run mutation tests, check test quality, or find untested code paths.
argument-hint: "'diff' for changed lines only (fast), 'full' for entire workspace (slow), or omit to be asked"
---

Check if `cargo-mutants` is installed by running `cargo mutants --version`. If the command fails or is not found, install it: `cargo install cargo-mutants --locked`. Wait for installation to complete before continuing.

## Choose a mode

If `$ARGUMENTS` is `diff`, use **Diff mode**. If `$ARGUMENTS` is `full`, use **Full mode**. Otherwise, ask the user:

> Which mutation testing mode?
> - **diff** — only mutates lines changed relative to `main`. Fast (seconds to a few minutes). Good for checking a branch's changes.
> - **full** — mutates the entire workspace. Slow (15–60 min). Good for a baseline coverage audit.

Wait for the user's answer before proceeding.

## Run the chosen mode

**Diff mode:**

```
git diff main > /tmp/ns2-mutation.diff && cargo mutants --workspace --in-diff /tmp/ns2-mutation.diff -vV; rm -rf mutants.out.old
```

If the diff file is empty (nothing changed relative to `main`), report that and stop — there are no mutations to test.

**Full mode:**

```
cargo mutants --workspace -j 4 -vV; rm -rf mutants.out.old
```

Warn the user this may take 15–60 minutes before starting. Capture the full output.

## Interpret the results

Parse the output for three categories of mutant outcomes:

- **Caught** — a test failed when the mutation was applied (good; test suite detected the bug)
- **Missed** — all tests passed despite the mutation (bad; a plausible bug went undetected)
- **Timeout** — the test suite did not finish within the time limit when the mutation was applied

### Missed mutants

For each missed mutant, explain concretely what the mutation was and why it matters. Do not just restate the diff — describe the real-world consequence. Example pattern:

> `harness/src/turn.rs:42`: The condition `status == Running` was replaced with `status != Running`. No test caught this. This means no test verifies the behavior when the session is *not* running — a future bug in that branch would go undetected.

Then suggest a specific test to add. Be concrete: name the function under test, describe the setup, and state what assertion would catch this mutation.

### Timeout mutants

List each timeout mutant by file and line. Note that timeouts often indicate a mutation introduced an infinite loop or blocking call. Flag these as worth investigating separately — they may point to missing guards or missing loop-termination tests.

## Output format

Group missed mutants by crate. For each crate, compute a mutation score:

```
<crate-name>: <caught> / <total> caught (<score>%)
```

Where `<total>` = caught + missed (exclude timeouts from the score denominator, but list them separately).

List missed mutants under their crate with file, line, the mutation applied, and the suggested test.

Close with a summary table:

| Crate | Caught | Missed | Timeouts | Score |
|-------|--------|--------|----------|-------|

Then a brief **Overall assessment**: whether the mutation score is acceptable, which crates have the weakest coverage, and the highest-priority tests to add based on the missed mutants found.
