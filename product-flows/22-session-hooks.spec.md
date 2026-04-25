---
targets:
  - crates/agents/src/**/*.rs
  - crates/harness/src/**/*.rs
verified: 2026-04-25T21:22:22Z
---

# Flow 22: Session Lifecycle Hooks (PreToolUse / PostToolUse / Stop)

Agent definitions may declare lifecycle hooks that the harness runs at specific points:

- **PreToolUse** — before each tool call; can block the tool by exiting non-zero
- **PostToolUse** — after each tool call (informational; exit code is ignored)
- **Stop** — when the agent turn loop ends (after `stop_reason == "end_turn"`); can block
  completion by exiting non-zero and providing a message back to the model

When an agent has `include_project_config: true`, it also inherits any hooks defined in
`.claude/settings.json` at the project root. Agent-level hooks take precedence over
project-level hooks for the same event type and matcher.

Hook schema matches Claude Code's schema: each hook entry has `type: "command"`, `command`,
optional `timeout` (seconds), optional `statusMessage`. The harness passes a JSON payload via
stdin to each hook command.

## Prerequisites

No API key required. The server is started without `ANTHROPIC_API_KEY` so the stub client is
used. The stub always returns `stop_reason: "end_turn"`.

## Fixture Setup

```bash
docker exec ns2-flow-22 bash /fixtures/init.sh
docker exec ns2-flow-22 bash /fixtures/start-server.sh
```

## Steps

### Step 1: Create a PreToolUse hook that logs tool calls

```bash
docker exec ns2-flow-22 bash -c 'mkdir -p /repo/scripts && cat > /repo/scripts/log-tool.sh <<'"'"'EOF'"'"'
#!/usr/bin/env bash
INPUT=$(cat)
TOOL=$(printf "%s" "$INPUT" | jq -r ".tool_name // \"unknown\"")
echo "pre-tool: $TOOL" >> /tmp/hook-log.txt
exit 0
EOF
chmod +x /repo/scripts/log-tool.sh'
```

Expected: script written and made executable.

### Step 2: Create an agent with a PreToolUse hook

```bash
docker exec ns2-flow-22 bash -c 'mkdir -p /repo/.ns2/agents && cat > /repo/.ns2/agents/hooked-agent.md <<'"'"'EOF'"'"'
---
name: hooked-agent
description: Agent with PreToolUse and PostToolUse hooks
hooks:
  PreToolUse:
    - matcher: ".*"
      hooks:
        - type: command
          command: /repo/scripts/log-tool.sh
          timeout: 10
---

You are a helpful assistant. Use the bash tool to run: echo hooked
EOF'
```

Expected: file written without error.

### Step 3: Start a session with `hooked-agent`

```bash
docker exec ns2-flow-22 bash -c 'cd /repo && ns2 session new --agent hooked-agent --message "run the bash tool" > /tmp/sess_hook.txt && cat /tmp/sess_hook.txt'
```

Expected: a UUID is printed.

```bash
docker exec ns2-flow-22 bash -c 'ns2 session tail --id "$(cat /tmp/sess_hook.txt)"'
```

Expected: output contains `[done]`.

### Step 4: Verify PreToolUse hook was invoked

```bash
docker exec ns2-flow-22 bash -c 'cat /tmp/hook-log.txt'
```

Expected: at least one line beginning with `pre-tool:`.

### Step 5: Create a PreToolUse hook that blocks a tool

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/scripts/block-tool.sh <<'"'"'EOF'"'"'
#!/usr/bin/env bash
INPUT=$(cat)
TOOL=$(printf "%s" "$INPUT" | jq -r ".tool_name // \"\"")
if [ "$TOOL" = "bash" ]; then
    echo "Bash tool is blocked by policy." >&2
    exit 2
fi
exit 0
EOF
chmod +x /repo/scripts/block-tool.sh'
```

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/.ns2/agents/blocked-agent.md <<'"'"'EOF'"'"'
---
name: blocked-agent
description: Agent whose bash tool is blocked
hooks:
  PreToolUse:
    - matcher: "bash"
      hooks:
        - type: command
          command: /repo/scripts/block-tool.sh
          timeout: 10
---

You are a helpful assistant.
EOF'
```

Expected: agent written without error.

### Step 6: Verify a blocked PreToolUse hook causes the harness to skip the tool

```bash
docker exec ns2-flow-22 bash -c 'cd /repo && ns2 session new --agent blocked-agent --message "run bash: echo hello" > /tmp/sess_blocked.txt && ns2 session tail --id "$(cat /tmp/sess_blocked.txt)"'
```

Expected: output contains `[done]` (session completes). The tool result fed back to the model
contains the hook's blocking message (`Bash tool is blocked by policy.`) rather than running the
actual command.

### Step 7: Create a Stop hook that always allows stopping

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/scripts/stop-allow.sh <<'"'"'EOF'"'"'
#!/usr/bin/env bash
echo "stop hook ran" >> /tmp/stop-log.txt
exit 0
EOF
chmod +x /repo/scripts/stop-allow.sh'
```

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/.ns2/agents/stop-agent.md <<'"'"'EOF'"'"'
---
name: stop-agent
description: Agent with a Stop hook that allows stopping
hooks:
  Stop:
    - hooks:
        - type: command
          command: /repo/scripts/stop-allow.sh
          timeout: 10
---

You are a helpful assistant.
EOF'
```

```bash
docker exec ns2-flow-22 bash -c 'cd /repo && ns2 session new --agent stop-agent --message "hello" > /tmp/sess_stop.txt && ns2 session tail --id "$(cat /tmp/sess_stop.txt)"'
```

Expected: `[done]` in output.

```bash
docker exec ns2-flow-22 bash -c 'cat /tmp/stop-log.txt'
```

Expected: contains `stop hook ran`.

### Step 8: Project-level hooks are inherited when `include_project_config: true`

```bash
docker exec ns2-flow-22 bash -c 'mkdir -p /repo/.claude/hooks && cat > /repo/.claude/hooks/proj-hook.sh <<'"'"'EOF'"'"'
#!/usr/bin/env bash
echo "project-hook ran" >> /tmp/proj-hook-log.txt
exit 0
EOF
chmod +x /repo/.claude/hooks/proj-hook.sh'
```

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/.claude/settings.json <<'"'"'EOF'"'"'
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": ".*",
        "hooks": [
          {"type": "command", "command": "/repo/.claude/hooks/proj-hook.sh", "timeout": 10}
        ]
      }
    ]
  }
}
EOF'
```

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/.ns2/agents/proj-config-agent.md <<'"'"'EOF'"'"'
---
name: proj-config-agent
description: Agent that inherits project-level hooks
include_project_config: true
---

You are a helpful assistant. Use the bash tool to run: echo inherited
EOF'
```

```bash
docker exec ns2-flow-22 bash -c 'cd /repo && ns2 session new --agent proj-config-agent --message "run bash tool" > /tmp/sess_proj.txt && ns2 session tail --id "$(cat /tmp/sess_proj.txt)"'
```

Expected: `[done]` in output.

```bash
docker exec ns2-flow-22 bash -c 'cat /tmp/proj-hook-log.txt'
```

Expected: contains `project-hook ran` — confirming the project-level PostToolUse hook was
inherited.

### Step 9: Agent-level hooks take precedence over project-level hooks for the same event+matcher

```bash
docker exec ns2-flow-22 bash -c 'cat > /repo/.ns2/agents/override-agent.md <<'"'"'EOF'"'"'
---
name: override-agent
description: Agent whose hooks override project hooks for the same event
include_project_config: true
hooks:
  PostToolUse:
    - matcher: ".*"
      hooks:
        - type: command
          command: /repo/scripts/log-tool.sh
          timeout: 10
---

You are a helpful assistant. Use bash tool: echo override
EOF'
```

```bash
docker exec ns2-flow-22 bash -c 'rm -f /tmp/proj-hook-log.txt /tmp/hook-log.txt && cd /repo && ns2 session new --agent override-agent --message "run bash" > /tmp/sess_ov.txt && ns2 session tail --id "$(cat /tmp/sess_ov.txt)"'
```

Expected: `[done]`.

```bash
docker exec ns2-flow-22 bash -c 'cat /tmp/hook-log.txt && echo "proj log:"; cat /tmp/proj-hook-log.txt 2>/dev/null || echo "(absent)"'
```

Expected: `/tmp/hook-log.txt` contains `pre-tool:`; `/tmp/proj-hook-log.txt` is absent or empty
(agent-level PostToolUse replaced the project-level one for the same matcher).

## Acceptance Criteria

- [ ] `AgentDef` supports a `hooks` field in frontmatter; missing field means no hooks
- [ ] Hooks are parsed from YAML frontmatter following the schema: `hooks: { EventType: [ { matcher, hooks: [ { type, command, timeout? } ] } ] }`
- [ ] Stop hooks have no `matcher` field (they apply unconditionally)
- [ ] **PreToolUse**: harness runs the hook command before each tool call; if exit code ≥ 1, the tool is skipped and hook stderr is returned as the tool result
- [ ] **PostToolUse**: harness runs the hook command after each tool call; exit code is always ignored
- [ ] **Stop**: harness runs the hook after `stop_reason == "end_turn"`; if exit code ≥ 1, hook stdout is injected as a new user message and the turn loop continues
- [ ] Hook stdin receives a JSON payload: `{"tool_name": "...", "tool_input": {...}}` for tool hooks; `{"session_id": "..."}` for Stop hooks
- [ ] When `include_project_config: true`, hooks from `.claude/settings.json` are loaded and merged; agent-level hooks for the same event type and matcher take precedence
- [ ] `.claude/settings.json` absence is not an error
- [ ] Hook timeout defaults to 60 seconds if not specified; commands that exceed timeout are killed and treated as if they returned exit code 1
- [ ] All hook invocations are non-blocking from the test suite's perspective (harness awaits them serially within a turn)

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.