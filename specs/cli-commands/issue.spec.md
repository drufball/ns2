---
targets:
  - crates/cli/src/commands/issue.rs
verified: 2026-04-29T17:13:15Z
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
ns2 issue start --id "$id"
ns2 issue wait --id "$id"
ns2 issue complete --id "$id" --comment "Done: added exponential backoff"
```

`issue new` prints the issue ID to stdout, making it easy to capture. `issue start` creates the session, sends the issue as the opening message, and links everything together. `issue wait` polls silently until the issue reaches a terminal state. `issue complete` requires `--comment` as a mandatory final summary.

You can also combine new and start with the `--start` flag on `ns2 issue new`.

## Listing and filtering

`ns2 issue list` shows all issues newest-first with their ID, title, status, assignee, and creation time. Filter by `--status`, `--assignee`, `--parent`, or `--blocked-on`.

## Reopening

`ns2 issue reopen --id <id>` moves a failed or completed issue back to open. The behavior differs by prior state: a **failed** issue gets its session cleared so the next start creates a fresh session; a **completed** issue keeps its session so history is resumed. Pass `--comment` to give the agent context before it picks back up, or `--start` to immediately kick off the next session.

## Orchestration

Issues support parent/child and blocking relationships for multi-agent workflows. Set `--parent` to nest an issue under another, and `--blocked-on` (repeatable) to declare that an issue can't start until its dependencies are complete. Filter `issue list` by these fields to navigate complex trees.