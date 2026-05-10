# Flow 05: Pluggable Issue Backends

Configure ns2 to use different issue storage backends: SQLite (default), GitHub Issues, or a user-defined shell script. Demonstrates backend configuration and the `ns2 event emit` integration point.

## Scope & Prerequisites

This flow requires **environment configuration** not available in standard CI smoke tests.

> **DO NOT run this flow as an automated smoke test.** It requires:
> - `NS2_GITHUB_WEBHOOK_SECRET=<secret>` in `.env`
> - GitHub webhook configured with a public server URL
> - `[issues] backend = "github"` in `ns2.toml`
> - `[issues.github] owner = "drufball" repo = "ns2"` in `ns2.toml`

For the SQLite and Shell backends, no external configuration is required.

## Setup (SQLite default — runs in CI)

```bash
/fixtures/init-git-repo.sh
/fixtures/copy-env.sh
cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &
sleep 3
/fixtures/create-swe-agent.sh
```

## SQLite Backend (Default)

### Step 1: Confirm default backend is sqlite

No `[issues]` section needed in `ns2.toml`. Create an issue to confirm normal operation:

```bash
ISSUE=$(ns2 issue new --title "SQLite test" --body "Verify default backend")
ns2 issue show --id "$ISSUE"
```

Expected: issue displays normally with status `open`.

### Step 2: Emit a custom event via CLI

```bash
ns2 event emit issue.status_changed '{"issue_id":"'"$ISSUE"'","from":"open","to":"in_progress"}'
```

Expected: exits 0. Event is emitted onto the EventBus. Hooks listening for `issue.status_changed` will fire.

## Shell Backend

### Step 3: Configure shell backend in ns2.toml

```toml
[issues]
backend = "shell"

[issues.shell]
command = ".ns2/backends/my-backend.sh"
```

### Step 4: Implement the shell backend script

The script receives a JSON envelope on stdin and must print a JSON response on stdout:

**Stdin envelope for `create`:**
```json
{"op": "create", "issue": {...}}
```
**Stdout response (success):**
```json
{"ok": true}
```

**Stdin envelope for `get`:**
```json
{"op": "get", "id": "ab12"}
```
**Stdout response:**
```json
{"ok": true, "issue": {...}}
```

**Stdin envelope for `list`:**
```json
{"op": "list", "filter": {"status": "open", "assignee": null, "parent_id": null}}
```
**Stdout response:**
```json
{"ok": true, "issues": [...]}
```

**Stdin envelope for `save`:**
```json
{"op": "save", "issue": {...}}
```

**Stdin envelope for `delete`:**
```json
{"op": "delete", "id": "ab12"}
```

**Error response (any op):**
```json
{"ok": false, "error": "not found"}
```

### Step 5: Verify shell backend routes CRUD correctly

```bash
ISSUE=$(ns2 issue new --title "Shell backend test" --body "Verify shell backend")
ns2 issue list --status open
ns2 issue show --id "$ISSUE"
```

Expected: All operations route through the shell script and return correct results.

## GitHub Backend (manual only)

### Step 6: Configure GitHub backend in ns2.toml

```toml
[issues]
backend = "github"

[issues.github]
owner = "drufball"
repo  = "ns2"
```

Add to `.env` (gitignored):
```bash
NS2_GITHUB_WEBHOOK_SECRET=<your-webhook-secret>
```

### Step 7: Verify GitHub issue creation

```bash
ISSUE=$(ns2 issue new --title "Test from ns2" --body "Created via GitHub backend")
```

Expected: A GitHub issue is created in the configured repo. The ns2 4-char ID is the canonical ID; a mapping table in SQLite tracks `github_issue_number → ns2_id`.

### Step 8: Verify status syncs via labels

```bash
ns2 issue set-status --id "$ISSUE" --status in_progress
```

Expected: The GitHub issue gains a label `ns2-status:in_progress`.

## Human vs Agent Assignment

### Step 9: Human assignee skips harness spawn

When an issue is assigned to a name without a corresponding `.ns2/agents/<name>.md` file, `start_issue` transitions the issue to `in_progress` without spawning a harness session. This is the integration point for external systems (GitHub users, Jira, etc.) handling the work.

```bash
ns2 issue new --title "Human task" --body "For a human" --assignee human-dev --status in_progress
ns2 issue show --id "$ISSUE"
```

Expected: issue shows status `in_progress`, `session_id` is `null`. No harness process spawned.

## Acceptance Criteria

### SQLite backend (default)
- [ ] No `[issues]` config required; sqlite is used automatically
- [ ] All existing issue lifecycle tests pass unchanged

### `ns2 event emit` CLI command
- [ ] `ns2 event emit <event-type> [payload-json]` exits 0 and emits the event onto the bus
- [ ] Missing payload-json defaults to `{}`
- [ ] Invalid JSON payload exits non-zero with error

### Shell backend
- [ ] `[issues] backend = "shell"` routes all CRUD through the configured script
- [ ] Script receives JSON on stdin; stdout JSON is parsed back to `Issue` types
- [ ] Error response (`{"ok": false, "error": "..."}`) surfaces as a proper Rust error
- [ ] Non-zero exit code from script treated as an error

### GitHub backend
- [ ] GitHub REST API used for issue CRUD
- [ ] ns2 4-char ID remains canonical; SQLite mapping table tracks `github_issue_number → ns2_id`
- [ ] Status carried via `ns2-status:<status>` labels
- [ ] Assignee carried via `ns2-assignee:<name>` labels
- [ ] `NS2_GITHUB_WEBHOOK_SECRET` required in `.env` when backend = "github"
- [ ] `.env.example` documents the required secret

### Human vs agent assignment
- [ ] If `.ns2/agents/<assignee>.md` does NOT exist, `start_issue` sets status to `in_progress` without spawning a harness
- [ ] If `.ns2/agents/<assignee>.md` DOES exist, existing harness-spawn behavior is unchanged
