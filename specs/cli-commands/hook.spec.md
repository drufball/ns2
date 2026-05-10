---
targets:
  - crates/cli/src/commands/hook.rs
  - crates/cli/src/main.rs
verified: 2026-05-10T17:00:01Z
---

# ns2 hook

Hooks react to system events and fire actions. A hook listens for one or more **named events** (identified by their `event_names` list). Common built-in event names:

- `issue.created`, `issue.status_changed`, `issue.comment_added` — internal issue lifecycle events
- `session.done` — session completed
- `external.<name>` — fired when a named webhook event is received (e.g. `external.ci-complete`)
- `timer.<name>` — fired by the timer scheduler for a named timer event (e.g. `timer.heartbeat`)
- `*` — matches all events

## Creating hooks

```bash
# React to an issue status change
ns2 hook new --name notify --event issue.status_changed \
  --action send-message --target issue:<id> \
  --body "Status is now {{ event.data.to }}"

# React to a named external webhook event
ns2 hook new --name ci-handler --event external.ci-complete \
  --action send-message --target issue:<id> --body "CI done"

# React to a named timer event
ns2 hook new --name ticker --event timer.heartbeat \
  --action send-message --target issue:<id> --body "tick"

# React to multiple events
ns2 hook new --name multi --event issue.created --event issue.status_changed \
  --action send-message --target issue:<id> --body "{{ event.data }}"
```

`--event` (repeatable) specifies the event names to listen for. Use `--event '*'` to match all events.

`--action` is always required: `send-message`, `create-issue`, or `run-shell`.

`--target` is required for `send-message`: `issue:<id>` routes the message as a comment on the specified issue.

`--body` supports minijinja templates with `{{ event.* }}` access to the triggering event payload.

## Listing and inspecting

```bash
ns2 hook list              # all hooks
ns2 hook list --enabled    # only enabled hooks
ns2 hook show --id <id>
ns2 hook logs --id <id>    # recent executions (default limit: 20)
```

## Enabling and disabling

```bash
ns2 hook enable --id <id>
ns2 hook disable --id <id>
ns2 hook delete --id <id>
```

## Subscribe shortcut

`ns2 issue subscribe` is sugar for creating a hook that watches a specific issue for status changes and new comments:

```bash
ns2 issue subscribe --id <issue-id> --deliver-to issue:<watcher-issue-id>
```