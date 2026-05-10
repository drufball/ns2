---
targets:
  - crates/cli/src/commands/agent.rs
verified: 2026-05-10T11:07:19Z
---

# ns2 agent

Agents define how a session behaves. Each agent is a Markdown file stored under `.ns2/agents/` in the repo root. The file has three fields: a `name` (used as the identifier), a `description` (one-line summary shown in listings), and a body (the system prompt loaded at the start of every session of that type). An agent with an empty body runs with no system prompt.

## Discovering agents

`ns2 agent list` shows all available agents — their names and descriptions. Run this to find valid values for `--agent` when starting a session or issue. The name shown in the list is exactly what you pass to `--agent`.

## Creating and updating agents

`ns2 agent new` creates a new agent file at `.ns2/agents/<name>.md`. Pass `--name`, `--description`, and `--body` as flags. In non-interactive scripts, always include `--body` — omitting it opens `$EDITOR` and blocks until you save and quit.

`ns2 agent edit` updates an existing agent in place. Only the fields you provide are changed; everything else stays the same. At least one of `--description` or `--body` is required.

## Example

```bash
ns2 agent list                           # see what's available
ns2 agent new --name qa --description "QA tester" --body "You are a QA engineer..."
ns2 agent edit --name qa --body "Updated system prompt"
```