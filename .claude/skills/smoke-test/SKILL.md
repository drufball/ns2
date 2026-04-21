---
name: smoke-test
description: Run product-flow manual tests sequentially with a clean slate per flow. Use when asked to smoke test, run manual tests, or validate product flows.
argument-hint: "flow numbers e.g. '01 03', or omit for all"
---

Run `product-flows/` as sequential subagents, one per flow. If `$ARGUMENTS` is given, run only those numbered flows; otherwise glob `product-flows/0*.md` in order.

## Per-flow (sequential)

Spawn a subagent per flow. Each subagent reads the flow file and follows it exactly — Setup, Steps, and Cleanup sections. Returns: pass/fail per acceptance criterion, any observations, and one of these verdicts:

- **PASS** — all criteria met
- **FAIL** — ran to completion, some criteria failed
- **CRITICAL** — infrastructure failure prevented full evaluation (server won't bind, binary crashes on every invocation); bail candidate
- **SKIPPED** — prerequisites not met (e.g. no `ANTHROPIC_API_KEY` for flow 04)

On CRITICAL: note it and continue unless it makes all remaining flows pointless, in which case bail with a reason.

If a failure is in testable business logic, write a failing unit test that reproduces it. If the failure is integration-only (CLI output format, server behaviour), note that instead.

While running, note anything anomalous that isn't captured by the acceptance criteria — unexpected output, timing that seems fragile, behaviour that passes today but looks load-bearing for a later flow, systemic signals (e.g. "turns are created faster than timestamp granularity can distinguish"). These become **Observations** in the per-flow report, separate from pass/fail. The goal is to surface latent issues before they become failures in a later flow.

Also note any **Workflow Snags** — friction that made the flow harder to execute or verify than it should have been. Examples: no way to wait for session completion without polling, opaque output that required workarounds to inspect, missing CLI commands that would have made a step straightforward, log output that was absent when a failure occurred. These are improvement signals for the developer environment and tooling, not product bugs.

## Report

Print a results table, then list every failed criterion with actual vs expected output underneath. Follow with an **Observations** section listing anything noted across flows — even from flows that passed. Then a **Workflow Snags** section listing friction points that made testing harder or reduced visibility, so they can be prioritised as tooling improvements.

| Flow | Name | Passed | Failed | Verdict |
|------|------|--------|--------|---------|
