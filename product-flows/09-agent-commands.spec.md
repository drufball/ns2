---
targets:
  - crates/agents/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-24T09:34:05Z
---


# Flow 09: Agent Commands

Create and manage agent type files using `ns2 agent list`, `ns2 agent new`, and `ns2 agent edit`.

These commands operate purely on the filesystem — no server required. Agent files live in
`.ns2/agents/` relative to the git root of the current repo.

## Prerequisites

No API key required. No server needed.

## Fixture Setup

```bash
docker exec ns2-flow-09 bash /fixtures/init.sh
```

The server is intentionally not started — agent commands are filesystem-only.

## Steps

### List agents when the directory does not exist yet

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent list'
```

Expected output:
```
No agents found (directory does not exist: /repo/.ns2/agents)
```

Exit code: 0. The `.ns2/agents/` directory does not exist yet; the command prints where it looked and exits cleanly rather than erroring.

### Create a first agent with all flags provided

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent new --name "reviewer" --description "Reviews pull requests for style and correctness" --body "You are a careful code reviewer. Focus on clarity and correctness."'
```

Expected output on stderr:
```
Created agent 'reviewer' at /repo/.ns2/agents/reviewer.md
```

Verify the file was written correctly:

```bash
docker exec ns2-flow-09 bash -c 'cat /repo/.ns2/agents/reviewer.md'
```

Expected file contents:
```
---
name: reviewer
description: Reviews pull requests for style and correctness
---

You are a careful code reviewer. Focus on clarity and correctness.
```

The file has YAML frontmatter followed by a blank line and the body text.

### List agents — shows the new entry

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent list'
```

Expected output — a two-column table with `name` padded to 20 characters:
```
name                 description
reviewer             Reviews pull requests for style and correctness
```

### Create a second agent

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent new --name "planner" --description "Breaks large tasks into actionable steps" --body "You are a planning assistant. Decompose problems into small, concrete steps."'
```

Expected output on stderr:
```
Created agent 'planner' at /repo/.ns2/agents/planner.md
```

### List shows both agents, sorted by name

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent list'
```

Expected output — agents appear in alphabetical order by name:
```
name                 description
planner              Breaks large tasks into actionable steps
reviewer             Reviews pull requests for style and correctness
```

### Edit the first agent's description

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent edit --name "reviewer" --description "Reviews code for correctness, style, and test coverage"'
```

Expected output on stderr:
```
Updated agent 'reviewer'.
```

Verify the description changed and the body was preserved:

```bash
docker exec ns2-flow-09 bash -c 'cat /repo/.ns2/agents/reviewer.md'
```

Expected file contents:
```
---
name: reviewer
description: Reviews code for correctness, style, and test coverage
---

You are a careful code reviewer. Focus on clarity and correctness.
```

### Edit the first agent's body

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent edit --name "reviewer" --body "You are a thorough code reviewer. Check for correctness, style, and adequate test coverage."'
```

Expected output on stderr:
```
Updated agent 'reviewer'.
```

Verify the body changed and the description from the previous step was preserved:

```bash
docker exec ns2-flow-09 bash -c 'cat /repo/.ns2/agents/reviewer.md'
```

Expected file contents:
```
---
name: reviewer
description: Reviews code for correctness, style, and test coverage
---

You are a thorough code reviewer. Check for correctness, style, and adequate test coverage.
```

## Error Cases

### `agent new` without `--name`

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent new --description "Missing name flag"; echo "Exit code: $?"'
```

Expected: error message and `Exit code: 1` (or any non-zero value). The `--name` flag is required.

### `agent edit` without `--name`

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent edit --description "No name given"; echo "Exit code: $?"'
```

Expected: error message and `Exit code: 1` (or any non-zero value). The `--name` flag is required.

### `agent edit` with `--name` but no other flags

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent edit --name "reviewer"'
```

Expected output (exit code non-zero):
```
Error: at least one of --description or --body must be provided
```

Nothing in the file changes.

### `agent new` with a duplicate name

```bash
docker exec ns2-flow-09 bash -c 'cd /repo && ns2 agent new --name "reviewer" --description "Duplicate" --body "This should fail."'
```

Expected output (exit code non-zero):
```
Error: agent 'reviewer' already exists at /repo/.ns2/agents/reviewer.md
```

The existing `reviewer.md` file is unchanged.

## Acceptance Criteria

- [ ] `ns2 agent list` exits 0 and prints `No agents found (directory does not exist: <path>)` when `.ns2/agents/` does not exist
- [ ] `ns2 agent list` exits 0 and prints `No agents found.` when `.ns2/agents/` exists but is empty
- [ ] `ns2 agent new` creates `.ns2/agents/` if it does not exist (no manual `mkdir` needed)
- [ ] `ns2 agent new --name <n> --description <d> --body <b>` creates `.ns2/agents/<n>.md` with correct YAML frontmatter and body
- [ ] Created file has a blank line between the closing `---` and the body text
- [ ] `ns2 agent new` prints `Created agent '<name>' at <path>` on stderr and exits 0
- [ ] `ns2 agent list` shows a two-column table with `name` padded to 20 characters and `description`
- [ ] `ns2 agent list` output is sorted alphabetically by name
- [ ] `ns2 agent edit --name <n> --description <d>` updates only the `description` frontmatter field, leaving the body intact
- [ ] `ns2 agent edit --name <n> --body <b>` replaces only the body, leaving frontmatter intact
- [ ] `ns2 agent edit` prints `Updated agent '<name>'.` on stderr and exits 0
- [ ] `ns2 agent new` without `--name` exits non-zero with an error message
- [ ] `ns2 agent edit` without `--name` exits non-zero with an error message
- [ ] `ns2 agent edit --name <n>` with no other flags exits non-zero with `Error: at least one of --description or --body must be provided`
- [ ] `ns2 agent new` with a name that already exists exits non-zero without overwriting the file
- [ ] None of these commands require a running server

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.