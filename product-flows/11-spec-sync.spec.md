---
targets:
  - crates/specs/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-24T14:12:36Z
---

# Flow 11: Spec Sync

Check whether source files covered by a spec have been modified since the spec was last
verified using `ns2 spec sync`. Exits non-zero if stale files are found.

These commands operate purely on the filesystem — no server required.

## Prerequisites

No API key required. No server needed.

## Fixture Setup

```bash
docker exec ns2-flow-11 bash /fixtures/init.sh
docker exec ns2-flow-11 bash /fixtures/codebase-layout.sh
```

The server is intentionally not started — spec commands are filesystem-only.

## Steps

### Set up: create a spec and verify it

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec new crates/cli/cli-commands.spec.md --target "crates/cli/src/**/*.rs"'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec verify crates/cli/cli-commands.spec.md'
```

### Sync passes when no files have been modified

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync crates/cli/cli-commands.spec.md; echo "Exit code: $?"'
```

Expected: no output and `Exit code: 0`. All target files were last modified before the `verified` timestamp.

### Touch a target file, then sync fails

```bash
docker exec ns2-flow-11 bash -c 'touch /repo/crates/cli/src/main.rs'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync crates/cli/cli-commands.spec.md; echo "Exit code: $?"'
```

Expected: error output listing the spec and the stale file, and `Exit code: 1`.

The output must include `crates/cli/cli-commands.spec.md` and `crates/cli/src/main.rs`.

### Sync all specs (no path argument)

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync; echo "Exit code: $?"'
```

Expected: same error output as above (finds all `.spec.md` files in the repo and checks each
one). `Exit code: 1` because the touched file is still stale.

### After verify, sync passes again

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec verify crates/cli/cli-commands.spec.md'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync crates/cli/cli-commands.spec.md; echo "Exit code: $?"'
```

Expected: no output and `Exit code: 0`.

### Unverified spec (no `verified` field) is always stale

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec new crates/agents/agents.spec.md --target "crates/agents/src/**/*.rs"'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync crates/agents/agents.spec.md; echo "Exit code: $?"'
```

Expected: error output listing all files matched by `crates/agents/src/**/*.rs`, and `Exit code: 1`.
A spec without a `verified` timestamp treats every matched file as stale.

### Spec files without valid frontmatter are silently skipped

The repo contains several `.spec.md` files without the `targets` frontmatter (e.g. the
architecture and harness spec files). Running `ns2 spec sync` without a path should not
error on those files — they are silently ignored.

```bash
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec verify crates/agents/agents.spec.md'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec verify crates/cli/cli-commands.spec.md'
docker exec ns2-flow-11 bash -c 'cd /repo && ns2 spec sync; echo "Exit code: $?"'
```

Expected: `Exit code: 0` (all specs with valid frontmatter are clean; legacy spec files are skipped).

## Acceptance Criteria

- [ ] `ns2 spec sync <path>` exits 0 with no output when no target files are stale
- [ ] `ns2 spec sync <path>` exits non-zero and lists stale files when any target file was modified after `verified`
- [ ] `ns2 spec sync` (no path) checks all `.spec.md` files found recursively from git root
- [ ] Spec files without a `targets` field in frontmatter are silently skipped
- [ ] A spec without a `verified` field treats all matched files as stale
- [ ] Error output includes the spec path and each stale file path
- [ ] No server required