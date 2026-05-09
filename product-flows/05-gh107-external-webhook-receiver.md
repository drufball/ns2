# Flow 05: GH#107 External Webhook Receiver HMAC Smoke Test

Smoke test for the external webhook receiver endpoint introduced in GH#107.
Tests HMAC signature verification, 401/404/200 response codes, and hook evaluator firing.

Branch under test: `hdk1-implement-gh-107-external-webhook-receiver-hmac`

## Prerequisites

- `ns2` binary compiled from the `hdk1-implement-gh-107-external-webhook-receiver-hmac` branch
- `openssl` available in the container
- `ANTHROPIC_API_KEY` set (for evaluator to send messages)

## Setup

```bash
# Create a temp working directory with a git repo (ns2 requires one)
mkdir -p /tmp/ns2-smoke-107 && cd /tmp/ns2-smoke-107
git init
git config user.email "test@test.com"
git config user.name "Test"
git commit --allow-empty -m "init"

# Start the server
ns2 server start --port 9877
sleep 2
```

## Steps

### Step 1: Create a watcher issue

```bash
cd /tmp/ns2-smoke-107
WATCHER_ID=$(ns2 issue new --title "Webhook Watcher" --body "watching for webhook events")
echo "Watcher issue: $WATCHER_ID"
```

Expected: a UUID printed to stdout.

### Step 2: Create an external hook with a secret

```bash
cd /tmp/ns2-smoke-107
HOOK_RESP=$(curl -s -X POST http://localhost:9877/hooks \
  -H 'Content-Type: application/json' \
  -d "{\"name\":\"ext-test\",\"source\":{\"type\":\"external\",\"secret\":\"test-secret\"},\"action\":{\"type\":\"send_message\",\"target\":{\"type\":\"issue\",\"content\":\"$WATCHER_ID\"},\"body\":\"Webhook fired: {{event.data.payload.action}}\"}}")
echo "$HOOK_RESP"
HOOK_ID=$(echo "$HOOK_RESP" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
echo "Hook ID: $HOOK_ID"
```

Expected: JSON response containing an `id` field with a UUID.

### Step 3: POST without signature → 401

```bash
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://localhost:9877/webhooks/$HOOK_ID \
  -H 'Content-Type: application/json' \
  -d '{"action":"opened"}')
echo "Status (no sig, expect 401): $STATUS"
```

Expected: `401`

### Step 4: POST with correct HMAC signature → 200

```bash
PAYLOAD='{"action":"opened"}'
SIG=$(echo -n "$PAYLOAD" | openssl dgst -sha256 -hmac "test-secret" | awk '{print "sha256="$2}')
echo "Signature: $SIG"
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://localhost:9877/webhooks/$HOOK_ID \
  -H 'Content-Type: application/json' \
  -H "X-Hub-Signature-256: $SIG" \
  -d "$PAYLOAD")
echo "Status (valid sig, expect 200): $STATUS"
```

Expected: `200`

### Step 5: POST to non-existent hook → 404

```bash
STATUS=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://localhost:9877/webhooks/xxxx \
  -H 'Content-Type: application/json' \
  -d '{}')
echo "Status (non-existent hook, expect 404): $STATUS"
```

Expected: `404`

### Step 6: Create no-secret hook and POST without signature → 200

```bash
cd /tmp/ns2-smoke-107
HOOK2_RESP=$(curl -s -X POST http://localhost:9877/hooks \
  -H 'Content-Type: application/json' \
  -d "{\"name\":\"ext-nosecret\",\"source\":{\"type\":\"external\"},\"action\":{\"type\":\"send_message\",\"target\":{\"type\":\"issue\",\"content\":\"$WATCHER_ID\"},\"body\":\"No-secret webhook fired: {{event.data.payload.action}}\"}}")
echo "$HOOK2_RESP"
HOOK2_ID=$(echo "$HOOK2_RESP" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
echo "No-secret Hook ID: $HOOK2_ID"

STATUS=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://localhost:9877/webhooks/$HOOK2_ID \
  -H 'Content-Type: application/json' \
  -d '{"action":"labeled"}')
echo "Status (no-secret hook, no sig, expect 200): $STATUS"
```

Expected: `200`

### Step 7: Verify hook evaluator fired (watcher issue has a comment)

Wait a few seconds for the evaluator to process, then check the issue comments.

```bash
sleep 5
cd /tmp/ns2-smoke-107
COMMENTS=$(ns2 issue comment list --id "$WATCHER_ID" 2>/dev/null || ns2 issue show --id "$WATCHER_ID")
echo "Comments/Issue output:"
echo "$COMMENTS"
```

Expected: The watcher issue should have at least one comment containing "Webhook fired:" or "No-secret webhook fired:" — confirming the hook evaluator ran.

## Cleanup

```bash
ns2 server stop
```

## Acceptance Criteria

- [ ] `ns2 server start --port 9877` starts without error
- [ ] `POST /hooks` with external source + secret creates a hook and returns a hook ID
- [ ] `POST /webhooks/:hook_id` without `X-Hub-Signature-256` returns **401**
- [ ] `POST /webhooks/:hook_id` with correct HMAC-SHA256 signature returns **200**
- [ ] `POST /webhooks/xxxx` (non-existent hook) returns **404**
- [ ] External hook with no secret configured accepts POST without signature → **200**
- [ ] Hook evaluator fires after valid webhook POST (watcher issue receives a comment)
- [ ] No panics or stack traces in server output throughout the test
