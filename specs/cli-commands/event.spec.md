---
targets:
  - crates/cli/src/commands/event.rs
  - crates/cli/src/main.rs
  - crates/server/src/routes/named_event.rs
  - crates/server/src/routes/emit.rs
verified: 2026-06-10T00:00:00Z
---

# ns2 event

The `ns2 event` command has two distinct roles:

1. **Named events** — webhook and timer triggers that hooks listen for.
2. **`ns2 event emit`** — injects a custom `SystemEvent::Custom` onto the EventBus directly.

## Named events

Named events are the triggers that hooks listen for. An event has a name, a type (webhook or timer), and optional metadata. Events are managed independently of hooks — create the event first, then create hooks that reference its name.

Two types:

- **webhook** — receives `POST /webhooks/:event_id` requests; hooks subscribe via `external.<name>`
- **timer** — fires on a recurring cron schedule; hooks subscribe via `timer.<name>`

### Creating events

```bash
# Webhook event (optional HMAC secret for signature verification)
ns2 event new ci-complete --type webhook --secret abc123

# Webhook event without a secret (no signature verification)
ns2 event new deploy-done --type webhook

# Timer event (5-field cron: minute hour dom month dow)
ns2 event new heartbeat --type timer --schedule "* * * * *"

# With an optional description
ns2 event new nightly-build --type timer --schedule "0 2 * * *" \
  --description "Nightly build trigger"
```

`--type` is required: `webhook` or `timer`.

`--schedule` is required when `--type timer` is used. The schedule is a standard 5-field cron expression. Invalid schedules are rejected by the server with a `400` error.

`--secret` is optional for webhook events. When set, incoming requests must include an `X-Hub-Signature-256: sha256=<hex>` header matching the HMAC-SHA256 of the body.

The command prints the new event ID to stdout (useful for scripting).

### Listing and deleting

```bash
ns2 event list
ns2 event delete --id <id>
```

`ns2 event list` shows a table with ID, name, type, enabled status, and creation time.

### Server routes

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/named-events` | Create a named event |
| `GET` | `/named-events` | List all named events |
| `GET` | `/named-events/:id` | Get a named event by ID |
| `DELETE` | `/named-events/:id` | Delete a named event |
| `POST` | `/webhooks/:event_id` | Receive an external webhook payload |

### Connecting events to hooks

After creating an event, create hooks that reference its name:

```bash
id=$(ns2 event new ci-complete --type webhook --secret s3cr3t)
ns2 hook new --name on-ci --event "external.ci-complete" \
  --action send-message --target issue:<id> --body "CI finished"
```

For timers the hook event name is `timer.<event-name>`:

```bash
ns2 event new heartbeat --type timer --schedule "*/5 * * * *"
ns2 hook new --name tick --event "timer.heartbeat" \
  --action send-message --target issue:<id> --body "ping"
```

## ns2 event emit

`ns2 event emit <event-type> [payload-json]` posts a custom event directly onto the ns2 EventBus.

```bash
# Emit with no payload
ns2 event emit custom.deploy-done

# Emit with a JSON payload
ns2 event emit custom.deploy-done '{"status": "ok", "sha": "abc123"}'
```

- `event-type` is required. Any string is accepted (e.g. `custom.test`, `issue.status_changed`).
- `payload-json` is optional. When provided it must be valid JSON; invalid JSON exits non-zero with an error message.
- On success the command exits 0 silently (no output).

**Server route:** `POST /events/emit` with body `{ "type": "<event-type>", "payload": <payload> }`.

The event is emitted as `SystemEvent::Custom { event_type, payload }`, which SSE clients and hooks can subscribe to. This is the primary inbound integration point for shell backends (see `[issues] backend = "shell"` in `ns2.toml`).
