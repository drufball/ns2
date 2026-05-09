---
targets:
  - crates/cli/src/commands/issue.rs
verified: 2026-05-09T06:32:23Z
---

# ns2 issue

Issues are the primary way to get work done. An issue is a lightweight work item with a title, body, optional assignee agent, and a status. Starting an issue automatically creates a session and hands it the title and body as the opening prompt.

## Lifecycle

- **open** — created, not yet started
- **running** — an agent session is actively working on it
- **completed** — work finished and reviewed
- **failed** — session ended with an error

## Typical workflow

```bash
id=$(ns2 issue new --title "Add retry logic" --body "..." --assignee swe)
ns2 issue set-status --id "$id" --status in_progress
ns2 issue wait --id "$id"
ns2 issue complete --id "$id" --comment "Done: added exponential backoff"
```

`issue new` prints the issue ID to stdout, making it easy to capture. `issue set-status --status in_progress` starts the assigned agent — it creates the session, sends the issue as the opening prompt, and links everything together. `issue wait` polls silently until the issue reaches a terminal state. `issue complete` requires `--comment` as a mandatory final summary.

You can combine creation and start in a single command:

```bash
id=$(ns2 issue new --title "Add retry logic" --body "..." --assignee swe \
       --status in_progress --wait)
```

`--status in_progress` sets the status (starting the agent) immediately after creation. `--wait` blocks until the issue reaches a terminal state and always prints the issue ID to stdout last. `--watch` streams live SSE events to stderr while `--wait` runs, keeping stdout capturable.

## Setting status and starting

`ns2 issue set-status --id <id> --status <status>` updates the issue status via `PATCH /issues/:id/status`. When `--status in_progress` is passed, the server auto-starts the issue: it validates that an assignee is set, and then either creates a fresh session (for open/failed issues) or resumes the existing session (for waiting issues). The issue moves to `running` — `in_progress` is only an input signal, never a stored state.

## Listing and filtering

`ns2 issue list` shows all issues newest-first with their ID, title, status, assignee, and creation time. Filter by `--status`, `--assignee`, `--parent`, or `--blocked-on`.

## Reopening

`ns2 issue reopen --id <id>` moves a failed, completed, or waiting issue back to open. The behavior differs by prior state: a **failed** issue gets its session cleared so the next start creates a fresh session; a **completed** or **waiting** issue keeps its session so history is resumed. Pass `--comment` to give the agent context before it picks back up.

## Orchestration

Issues support parent/child and blocking relationships for multi-agent workflows. Set `--parent` to nest an issue under another, and `--blocked-on` (repeatable) to declare that an issue can't start until its dependencies are complete. Filter `issue list` by these fields to navigate complex trees.