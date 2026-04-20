# Flow 03: Hello World (Stub)

Full session lifecycle without a real API key. The server returns a hardcoded stub response.

## Setup

Start the server **without** `ANTHROPIC_API_KEY` set. Unset the variable if it was loaded from `.env`:

```bash
source product-flows/setup.sh
cd /tmp/ns2-test-repo
unset ANTHROPIC_API_KEY
$NS2 server start &
```

Confirm the key is absent:
```bash
echo "${ANTHROPIC_API_KEY:-not set}"
```
Expected: `not set`

## Steps

### Create a session with a message

```bash
SESSION_ID=$($NS2 session new --message "hello")
echo "Session: $SESSION_ID"
```

Expected: a UUID printed and stored in `SESSION_ID`.

### Tail the session

```bash
$NS2 session tail --id "$SESSION_ID"
```

The command streams events and exits when the session reaches `completed`. This may take a second.

### Expected output

```
[assistant] Hello! I'm a stub assistant.
Session completed.
```

The exact phrasing of the stub message may differ slightly, but it must not be empty and must not be a Claude API response.

### Verify session status

```bash
$NS2 session list --status completed
```

Expected: the session ID appears in the list with status `completed`.

## Acceptance Criteria

- [ ] `ns2 session new --message "hello"` succeeds without `ANTHROPIC_API_KEY` set
- [ ] `ns2 session tail` streams at least one text event from the stub
- [ ] The stub response contains "stub" or similar indication it is not a real API call
- [ ] The session transitions to `completed` after the stub response
- [ ] `ns2 session list --status completed` shows the session
- [ ] No panics, stack traces, or unhandled errors in server output

## Cleanup

```bash
bash product-flows/cleanup.sh
```
