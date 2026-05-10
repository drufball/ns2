---
targets:
  - crates/cli/src/commands/hook.rs
  - crates/cli/src/main.rs
verified: 2026-05-10T11:09:26Z
---

# ns2 hook

Hooks react to system events and fire actions. There are three source types:

- **internal** — fires when a `SystemEvent` matches the hook's `event_types` and optional filter
- **external** — fires when `POST /hooks/:id/trigger` is called directly
- **timer** — fires on a recurring cron schedule

## Creating hooks

```bash
# Internal hook: post a comment when an issue status changes
ns2 hook new --name notify --source internal \
  --event-type issue.status_changed \
  --action send-message --target issue:<id> \
  --body "Status is now {{ event.data.to }}"

# Timer hook: post every Monday at 9am UTC
ns2 hook new --name weekly --source timer \
  --schedule "0 9 * * 1" \
  --action send-message --target issue:<id> \
  --body "Weekly check-in"
```

`--schedule` is required when `--source timer` is used. The schedule is a standard 5-field cron expression (`minute hour dom month dow`). Invalid schedules are rejected by the server with a `400` error.

`--event-type` (repeatable) is required when `--source internal` is used. Common values: `issue.created`, `issue.status_changed`, `issue.comment_added`, `session.done`.

`--action` is always required: `send-message` (currently implemented), `create-issue`, or `run-shell` (both placeholder).

`--target` is required for `send-message`: `issue:<id>` routes the message as a comment on the specified issue.

`--body` supports minijinja templates with `{{ event.* }}` access to the triggering event payload.

## Listing and inspecting

```bash
ns2 hook list                        # all hooks
ns2 hook list --source-type timer    # only timer hooks
ns2 hook list --enabled true         # only enabled hooks
ns2 hook show --id <id>
ns2 hook logs --id <id>              # recent executions (default limit: 20)
```

## Enabling and disabling

```bash
ns2 hook enable --id <id>
ns2 hook disable --id <id>
ns2 hook delete --id <id>
```

## Subscribe shortcut

`ns2 hook subscribe` is sugar for creating an internal hook that watches a specific issue for status changes and new comments:

```bash
ns2 hook subscribe --id <issue-id> --deliver-to <watcher-issue-id>
```