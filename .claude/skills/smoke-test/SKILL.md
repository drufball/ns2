---
name: smoke-test
description: Run product-flow manual tests sequentially with a clean slate per flow. Use when asked to smoke test, run manual tests, or validate product flows.
argument-hint: "flow numbers e.g. '01 03', or omit for all"
---

Run `product-flows/` as sequential subagents, one per flow. If `$ARGUMENTS` is given, run only those numbered flows; otherwise glob `product-flows/0*.md` in order.

## Per-flow (sequential)

Spawn a subagent per flow. Each subagent reads the flow file and follows it exactly — Setup, Steps, and Cleanup sections. Returns: pass/fail per acceptance criterion + one of these verdicts:

- **PASS** — all criteria met
- **FAIL** — ran to completion, some criteria failed
- **CRITICAL** — infrastructure failure prevented full evaluation (server won't bind, binary crashes on every invocation); bail candidate
- **SKIPPED** — prerequisites not met (e.g. no `ANTHROPIC_API_KEY` for flow 04)

On CRITICAL: note it and continue unless it makes all remaining flows pointless, in which case bail with a reason.

If a failure is in testable business logic, write a failing unit test that reproduces it. If the failure is integration-only (CLI output format, server behaviour), note that instead.

## Report

Print a results table, then list every failed criterion with actual vs expected output underneath.

| Flow | Name | Passed | Failed | Verdict |
|------|------|--------|--------|---------|
