---
name: smoke-test
description: Run product-flow manual tests in parallel Docker containers, one per flow. Use when asked to smoke test, run manual tests, or validate product flows.
argument-hint: "flow numbers e.g. '01 03', or omit for all"
---

Run `product-flows/` flows in parallel Docker containers. If `$ARGUMENTS` is given, run only those numbered flows; otherwise run all flows 01–09.

## Step 0: Detect repo root and build

Detect the repo root:

```bash
git rev-parse --show-toplevel
```

All paths below use `REPO_ROOT` as a placeholder — substitute the actual path.

Run the build script from the repo root:

```bash
bash REPO_ROOT/product-flows/build.sh
```

If build.sh exits non-zero, bail immediately with the message: "Build failed — aborting smoke test." Do not proceed.

After build.sh completes, verify the binary exists:

```bash
ls REPO_ROOT/product-flows/.build/ns2
```

If the binary is missing, bail immediately with: "Binary not found at product-flows/.build/ns2 — aborting."

## Step 1: Check prerequisites

Check whether `ANTHROPIC_API_KEY` is set:

```bash
echo ${ANTHROPIC_API_KEY:-}
```

If the output is empty, `ANTHROPIC_API_KEY` is unset. Any flow whose Prerequisites section contains `[Requires: ANTHROPIC_API_KEY]` must be marked SKIPPED — do not start a container for it.

## Step 2: Start all containers

For each flow being run (and not SKIPPED), start a detached container before spawning any subagents. Container names follow the pattern `ns2-flow-NN` where NN is the two-digit flow number.

```bash
docker run -d \
  --name ns2-flow-NN \
  -v REPO_ROOT/product-flows/.build/ns2:/usr/local/bin/ns2:ro \
  -v REPO_ROOT/.env:/tmp/ns2-host.env:ro \
  -v REPO_ROOT/product-flows/fixtures:/fixtures:ro \
  ns2-test tail -f /dev/null
```

If `docker run` returns non-zero for a given flow, mark that flow CRITICAL, record the error, and do not spawn a subagent for it. Still run cleanup for it at the end.

## Step 3: Spawn all subagents simultaneously

After all containers are started, spawn one subagent per non-SKIPPED, non-CRITICAL flow. Spawn them all at the same time — do not wait for one to finish before starting the next. Each container has its own network namespace, so port 9876 does not conflict between flows.

Each subagent receives:
- The full text of the flow .md file
- The container name assigned to it (e.g. `ns2-flow-03`)
- This instruction: **All bash commands in this flow must be run via `docker exec <container-name> bash -c '...'`. Do not run commands on the host. The Fixture Setup section contains the setup commands already formatted as `docker exec` calls — run them exactly as written.**

Each subagent follows the flow file exactly:
1. Run the Fixture Setup commands (already formatted as `docker exec` calls)
2. Execute each Step in order, using `docker exec ns2-flow-NN bash -c '...'` for every command
3. Evaluate each Acceptance Criterion
4. Do not run Cleanup — the orchestrating agent handles container removal

Each subagent returns:
- Pass/fail for each acceptance criterion
- One verdict: PASS, FAIL, CRITICAL, or SKIPPED
- Observations (anomalies that aren't captured by acceptance criteria)
- Workflow Snags (friction that made the flow harder to execute or verify)

Verdict definitions:

- **PASS** — all criteria met
- **FAIL** — ran to completion, one or more criteria failed
- **CRITICAL** — infrastructure failure prevented full evaluation (container wouldn't exec, binary crashed on every invocation)
- **SKIPPED** — prerequisites not met

If a failure is in testable business logic, the subagent writes a failing unit test that reproduces it. If the failure is integration-only (CLI output format, server behaviour), it notes that instead.

Subagents note anything anomalous that isn't captured by the acceptance criteria — unexpected output, timing that seems fragile, behaviour that passes today but looks load-bearing for a later flow, systemic signals (e.g. "turns are created faster than timestamp granularity can distinguish"). These become **Observations** in the per-flow report, separate from pass/fail. The goal is to surface latent issues before they become failures in a later flow.

Subagents also note **Workflow Snags** — friction that made the flow harder to execute or verify than it should have been. Examples: no way to wait for session completion without polling, opaque output that required workarounds to inspect, missing CLI commands that would have made a step straightforward, log output that was absent when a failure occurred.

## Step 4: Wait for all subagents

Wait for every spawned subagent to finish before proceeding.

## Step 5: Cleanup all containers

For every flow that had a container started (including CRITICAL flows), run:

```bash
docker rm -f ns2-flow-NN
```

Run cleanup for each container regardless of that flow's outcome. Record whether each removal succeeded or failed — this populates the Container Cleanup Status section of the report.

## Report

Print the following sections in order.

### Results table

| Flow | Name | Passed | Failed | Verdict |
|------|------|--------|--------|---------|

### Failed criteria

For each FAIL or CRITICAL flow, list every failed criterion with actual vs expected output.

### Observations

List anything noted across all flows — even from flows that passed. Surface latent issues and anomalous behaviour.

### Workflow Snags

List friction points that made testing harder or reduced visibility. These are improvement signals for the developer environment and tooling, not product bugs.

### Container Cleanup Status

Confirm whether each container was successfully removed. Format:

| Container | Removed |
|-----------|---------|
| ns2-flow-01 | yes |
| ns2-flow-02 | yes |
| ... | ... |

If any removal failed, list the error.
