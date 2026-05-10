# GH#148 Smoke Test — --recursive flag for issue subscribe

## Context

Commit f1849af added `--recursive` flag to:
- `ns2 issue subscribe --id <id> --deliver-to issue:<id> --recursive`
- `ns2 issue new --subscribe issue:<id> --recursive`

## Setup

```bash
/fixtures/init-git-repo.sh
/fixtures/copy-env.sh
cd /tmp/ns2-smoke && nohup ns2 server start > /tmp/ns2-server.log 2>&1 &
sleep 3
/fixtures/create-swe-agent.sh
```

## Tests

```bash
cd /tmp/ns2-smoke

# Standard subscribe (should still work)
WATCHER=$(ns2 issue new --title "Watcher" --body "Watcher")
WORK=$(ns2 issue new --title "Work" --body "Work" --assignee swe)
HOOK=$(ns2 issue subscribe --id "$WORK" --deliver-to "issue:$WATCHER")
echo "Standard hook: $HOOK"  # expect 4-char ID on stdout
ns2 hook list  # expect subscribe-<WORK> hook

# Recursive subscribe
ROOT=$(ns2 issue new --title "Root" --body "Root task")
RHOOK=$(ns2 issue subscribe --id "$ROOT" --deliver-to "issue:$WATCHER" --recursive)
echo "Recursive hook: $RHOOK"  # expect 4-char ID on stdout
ns2 hook list  # expect subscribe-<ROOT>-recursive hook

# issue new --subscribe --recursive
CHILD=$(ns2 issue new --title "Child" --body "Child task" --subscribe "issue:$WATCHER" --recursive)
echo "Child: $CHILD"  # expect 4-char issue ID on stdout
ns2 hook list  # expect subscribe-<CHILD>-recursive hook
```

## Pass Criteria

1. **Standard hook output**: `ns2 issue subscribe --id "$WORK" --deliver-to "issue:$WATCHER"` prints a 4-char hook ID to stdout (no error)
2. **Standard hook listed**: `ns2 hook list` shows a hook named `subscribe-<WORK>` (non-recursive)
3. **Recursive hook output**: `ns2 issue subscribe --id "$ROOT" --deliver-to "issue:$WATCHER" --recursive` prints a 4-char hook ID to stdout (no error)
4. **Recursive hook listed**: `ns2 hook list` shows a hook named `subscribe-<ROOT>-recursive`
5. **issue new --subscribe --recursive output**: `ns2 issue new --title "Child" --body "Child task" --subscribe "issue:$WATCHER" --recursive` prints a 4-char issue ID to stdout (no error)
6. **Child recursive hook listed**: `ns2 hook list` shows a hook named `subscribe-<CHILD>-recursive`
