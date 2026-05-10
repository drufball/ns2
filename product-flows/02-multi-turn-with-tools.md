
# Flow 02: Multi-Turn Conversation with Tools

Claude writes and reads files using tools across multiple turns within a session. Combines tool-use and conversation-history verification in a single flow.

## Prerequisites

`ANTHROPIC_API_KEY` must be set in your shell.

## Setup

```bash
git init /tmp/ns2-smoke && cd /tmp/ns2-smoke
git commit --allow-empty -m "init"
echo "The magic number is: 7742" > multi-turn-test.txt
git add . && git commit -m "seed"
ns2 server start
```

## Fixture Setup

```bash
docker exec ns2-flow-02 bash -c 'mkdir -p /tmp/ns2-smoke && git -C /tmp/ns2-smoke init && echo "The magic number is: 7742" > /tmp/ns2-smoke/multi-turn-test.txt && git -C /tmp/ns2-smoke add . && git -C /tmp/ns2-smoke commit -m "seed"'
docker exec ns2-flow-02 bash -c 'set -a; . /tmp/ns2-host.env; set +a; cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &'
sleep 3
```

## Steps

### Part A: Tool Use (write + read)

#### Create a session asking Claude to write and then read a file

```bash
SESSION=$(ns2 session new --message "Please write the text 'ns2-multi-tool-test-99' to the file /tmp/ns2-multi-tool-test.txt, then read it back and tell me what it contains.")
echo "Session: $SESSION"
```

Expected: a UUID printed to stdout.

#### Tail the session and wait for completion

```bash
ns2 session tail --id "$SESSION"
```

Claude will invoke the `write` tool, then the `read` tool, then summarize the result.

Expected output shape:
```
[tool: write({"path":"/tmp/ns2-multi-tool-test.txt","content":"ns2-multi-tool-test-99"})]
[result: Wrote N bytes to /tmp/ns2-multi-tool-test.txt]
[tool: read({"path":"/tmp/ns2-multi-tool-test.txt"})]
[result: ns2-multi-tool-test-99]
The file contains: "ns2-multi-tool-test-99"
[done]
```

The exact phrasing varies, but Claude's final response must include `ns2-multi-tool-test-99`.

#### Verify the file was actually written

```bash
cat /tmp/ns2-multi-tool-test.txt
```

Expected: `ns2-multi-tool-test-99`

### Part B: Multi-Turn Conversation (session resume)

#### Start a session asking Claude to read the seeded file

```bash
SESSION2=$(ns2 session new --message "Please read the file at $(pwd)/multi-turn-test.txt and tell me what the magic number is.")
echo "Session: $SESSION2"
```

#### Tail the first run

```bash
ns2 session tail --id "$SESSION2"
```

Expected: Claude reads the file and reports `7742`. Session transitions to `completed`.

#### Send a follow-up message

```bash
ns2 session send --id "$SESSION2" --message "What was the magic number you found? Double it and tell me the result."
```

Expected: command exits 0.

#### Tail the session again

```bash
ns2 session tail --id "$SESSION2"
```

The output replays the first run's turns, then shows a second set for the follow-up. Claude's second response must:
- Reference `7742` from prior context (no new `read` tool call)
- State the doubled value: `15484`

#### Tail with --turns 1 to replay only the final turn

```bash
ns2 session tail --id "$SESSION2" --turns 1
```

Expected: only the last assistant turn is shown — no tool calls, no first-run content.

## Acceptance Criteria

**Tool use:**
- [ ] Claude invokes `write` and `read` tools correctly
- [ ] The string `ns2-multi-tool-test-99` appears in Claude's final response
- [ ] The file exists on disk with the correct content
- [ ] `session tail` shows `[tool: ...]` and `[result: ...]` lines for both tool calls

**Multi-turn:**
- [ ] `session send` on a `completed` session returns 200
- [ ] The second run has full conversation history (Claude knows `7742` without re-reading)
- [ ] Claude's second response contains `15484` (7742 × 2)
- [ ] `session tail --turns 1` replays only the final turn
- [ ] The session returns to `completed` after the second run

**General:**
- [ ] No panics or unhandled errors in server output
