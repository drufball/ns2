---
targets:
  - crates/agents/src/**/*.rs
  - crates/harness/src/**/*.rs
verified: 2026-04-26T16:15:47Z
---

# Flow 21: Project Config Inheritance (CLAUDE.md Loading)

When an agent definition has `include_project_config: true` in its frontmatter, the harness
loads `CLAUDE.md` from the repository root and appends its content (plus any `@`-imported files)
to the agent's system prompt before starting the session.

`@`-imports may be embedded anywhere in a line (e.g. `See @docs/style.md for details`).
Invalid imports produce a warning on stderr but do not abort the session.

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY` so the stub client is
used — sessions complete immediately with a canned response. What we verify is the system prompt
content captured from requests sent to the stub.

## Fixture Setup

```bash
docker exec ns2-flow-21 bash /fixtures/init.sh
docker exec ns2-flow-21 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Create a CLAUDE.md with an embedded @-import

```bash
docker exec ns2-flow-21 bash -c 'cat > /repo/CLAUDE.md <<'"'"'EOF'"'"'
# Project Config

This is the project guide. See @docs/style.md for style rules.

## Commands
cargo test
EOF'
```

```bash
docker exec ns2-flow-21 bash -c 'mkdir -p /repo/docs && cat > /repo/docs/style.md <<'"'"'EOF'"'"'
# Style Guide

Use snake_case everywhere.
EOF'
```

Expected: files written without error.

### Step 2: Create an agent with `include_project_config: true`

```bash
docker exec ns2-flow-21 bash -c 'mkdir -p /repo/.ns2/agents && cat > /repo/.ns2/agents/project-agent.md <<'"'"'EOF'"'"'
---
name: project-agent
description: Agent that loads CLAUDE.md into its system prompt
include_project_config: true
---

You are a helpful assistant.
EOF'
```

Expected: file written without error.

### Step 3: Create an agent without `include_project_config` (defaults to false)

```bash
docker exec ns2-flow-21 bash -c 'cat > /repo/.ns2/agents/plain-agent.md <<'"'"'EOF'"'"'
---
name: plain-agent
description: Agent that does NOT load CLAUDE.md
---

You are a plain assistant.
EOF'
```

Expected: file written without error.

### Step 4: Start a session with `project-agent` and capture the system prompt

```bash
docker exec ns2-flow-21 bash -c 'cd /repo && ns2 session new --agent project-agent --message "hello" > /tmp/sess_project.txt && cat /tmp/sess_project.txt'
```

Expected: a UUID is printed to stdout.

```bash
docker exec ns2-flow-21 bash -c 'ns2 session tail --id "$(cat /tmp/sess_project.txt)"'
```

Expected: output contains `[done]` (stub client completes immediately).

### Step 5: Verify the system prompt included CLAUDE.md content

```bash
docker exec ns2-flow-21 bash -c 'ns2 session tail --id "$(cat /tmp/sess_project.txt)" 2>&1 | grep -q "done" && echo "completed"'
```

Inspect the captured request body written by the test harness to confirm the system prompt
contains both the agent body and the CLAUDE.md content:

```bash
docker exec ns2-flow-21 bash -c 'cat /tmp/captured-system.txt'
```

Expected: `/tmp/captured-system.txt` contains `You are a helpful assistant.` AND
`This is the project guide.` AND `Use snake_case everywhere.`
(the agent body is prepended; CLAUDE.md body and its @-imported content follow).

### Step 6: Start a session with `plain-agent` and verify CLAUDE.md is NOT included

```bash
docker exec ns2-flow-21 bash -c 'cd /repo && ns2 session new --agent plain-agent --message "hello" > /tmp/sess_plain.txt && ns2 session tail --id "$(cat /tmp/sess_plain.txt)"'
```

Expected: output contains `[done]`.

```bash
docker exec ns2-flow-21 bash -c 'cat /tmp/captured-system.txt'
```

Expected: `/tmp/captured-system.txt` contains `You are a plain assistant.` but does NOT
contain `This is the project guide.`.

### Step 7: Verify @-import embedded in a line is resolved

```bash
docker exec ns2-flow-21 bash -c 'cat /tmp/captured-system.txt | grep -c "Use snake_case"'
```

Expected: output is `1` — the style guide was imported exactly once even though the `@`-import
appeared mid-line (`See @docs/style.md for style rules.`).

### Step 8: Invalid @-import warns but does not abort

```bash
docker exec ns2-flow-21 bash -c 'cat >> /repo/CLAUDE.md <<'"'"'EOF'"'"'

Also see @nonexistent/file.md for more info.
EOF'
```

```bash
docker exec ns2-flow-21 bash -c 'cd /repo && ns2 session new --agent project-agent --message "hello" 2>/tmp/sess_warn_err.txt > /tmp/sess_warn.txt && ns2 session tail --id "$(cat /tmp/sess_warn.txt)"'
```

Expected: session still completes (`[done]` in output). `cat /tmp/sess_warn_err.txt` contains a
warning mentioning `nonexistent/file.md` (or similar) — written to stderr, not aborting the
session.

```bash
docker exec ns2-flow-21 bash -c 'grep -i "warn\|import\|nonexistent" /tmp/sess_warn_err.txt'
```

Expected: at least one line matching the grep — a human-readable warning about the missing import.

## Acceptance Criteria

- [ ] `AgentDef` gains an `include_project_config: bool` field (defaults to `false` when absent from frontmatter)
- [ ] `parse_agent_content` parses `include_project_config: true/false` from frontmatter
- [ ] `format_agent_file` serializes `include_project_config` when `true` (omits the field when `false`)
- [ ] When `include_project_config` is `true`, the harness loads `CLAUDE.md` from the git root before building the system prompt
- [ ] The final system prompt is: `<agent body>\n\n<CLAUDE.md content>\n\n<imported file content>` (agent body first)
- [ ] `@path/to/file.md` references anywhere in a line in CLAUDE.md are resolved relative to git root and their content appended
- [ ] Each imported file is included at most once (dedup by path)
- [ ] If an `@`-imported file does not exist, a warning is emitted to stderr and the session continues
- [ ] If `CLAUDE.md` itself does not exist, a warning is emitted to stderr and the session continues with only the agent body
- [ ] A session with `include_project_config: false` (or absent) sends no CLAUDE.md content in system prompt
- [ ] Existing `AgentDef` parse/format round-trips remain valid (no regression)

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.