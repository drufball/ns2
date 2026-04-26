---
targets:
  - crates/specs/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-26T16:18:47Z
---

# Flow 12: Spec Verify

Mark a spec as verified at the current time using `ns2 spec verify`. Updates the `verified`
frontmatter field to the current UTC timestamp while preserving the rest of the file.
Multiple paths can be passed in a single invocation to verify several specs at once.

These commands operate purely on the filesystem — no server required.

## Prerequisites

No API key required. No server needed.

## Fixture Setup

```bash
docker exec ns2-flow-12 bash /fixtures/init.sh
```

The server is intentionally not started — spec commands are filesystem-only.

## Steps

### Create a spec (no verified timestamp yet)

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec new crates/cli/cli-commands.spec.md --target "crates/cli/src/**/*.rs"'
docker exec ns2-flow-12 bash -c 'cat /repo/crates/cli/cli-commands.spec.md'
```

Expected: file has `targets` list but no `verified` field.

### Verify the spec

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify crates/cli/cli-commands.spec.md'
```

Expected output on stdout:
```
Verified crates/cli/cli-commands.spec.md
```

### Verified field is written into frontmatter

```bash
docker exec ns2-flow-12 bash -c 'cat /repo/crates/cli/cli-commands.spec.md'
```

Expected: file now contains a `verified:` line in the frontmatter with an ISO 8601 UTC
timestamp (e.g. `verified: 2024-01-15T10:30:00Z`). The `targets` list is unchanged.

Example:
```
---
targets:
  - crates/cli/src/**/*.rs
verified: 2024-01-15T10:30:00Z
---
```

### Verify again updates the timestamp

```bash
docker exec ns2-flow-12 bash -c 'sleep 1 && cd /repo && ns2 spec verify crates/cli/cli-commands.spec.md'
docker exec ns2-flow-12 bash -c 'cat /repo/crates/cli/cli-commands.spec.md'
```

Expected: the `verified` timestamp in the file is later than the timestamp from the first
`verify` call. Targets are still unchanged.

### Body content is preserved across verify

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec new crates/agents/agents.spec.md --target "crates/agents/src/**/*.rs"'
```

Manually add body text to test preservation:

```bash
docker exec ns2-flow-12 bash -c 'printf "\n# My Spec\n\nThis describes something important.\n" >> /repo/crates/agents/agents.spec.md'
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify crates/agents/agents.spec.md'
docker exec ns2-flow-12 bash -c 'cat /repo/crates/agents/agents.spec.md'
```

Expected: file has the `verified` timestamp in frontmatter, and the body `# My Spec\n\nThis describes something important.` is preserved verbatim.

### Verify multiple specs in one invocation

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec new crates/tools/tools.spec.md --target "crates/tools/src/**/*.rs"'
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec new crates/db/db.spec.md --target "crates/db/src/**/*.rs"'
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify crates/tools/tools.spec.md crates/db/db.spec.md'
```

Expected output on stdout (one line per spec, order matching the arguments):
```
Verified crates/tools/tools.spec.md
Verified crates/db/db.spec.md
```

Both files must now contain a `verified:` timestamp in their frontmatter:

```bash
docker exec ns2-flow-12 bash -c 'grep verified /repo/crates/tools/tools.spec.md /repo/crates/db/db.spec.md'
```

Expected: each file shows a `verified: <ISO8601>` line.

### Multi-path verify: one path invalid, rest succeed, exit non-zero

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec new crates/server/server.spec.md --target "crates/server/src/**/*.rs"'
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify crates/server/server.spec.md crates/nonexistent/missing.spec.md crates/tools/tools.spec.md; echo "Exit code: $?"'
```

Expected: `Verified crates/server/server.spec.md` and `Verified crates/tools/tools.spec.md` are printed for the valid paths. An error message on stderr references `crates/nonexistent/missing.spec.md`. Exit code is `1` (at least one path failed).

## Error Cases

### `spec verify` on a non-existent path

```bash
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify crates/nonexistent/missing.spec.md; echo "Exit code: $?"'
```

Expected: error message on stderr and `Exit code: 1`.

### `spec verify` on a file without valid frontmatter

```bash
docker exec ns2-flow-12 bash -c 'echo "# just a plain markdown file" > /repo/plain.spec.md'
docker exec ns2-flow-12 bash -c 'cd /repo && ns2 spec verify plain.spec.md; echo "Exit code: $?"'
```

Expected: error message on stderr (invalid frontmatter or missing `targets`) and `Exit code: 1`.

## Acceptance Criteria

- [ ] `ns2 spec verify <path>` writes the current UTC timestamp into the `verified` frontmatter field
- [ ] `ns2 spec verify` prints `Verified <path>` on stdout and exits 0
- [ ] Running `verify` twice updates the timestamp to the later time
- [ ] `targets` list and body content are preserved unchanged after verify
- [ ] `ns2 spec verify <path1> <path2> ...` accepts multiple paths and verifies all of them
- [ ] Each verified path produces a `Verified <path>` line on stdout in argument order
- [ ] When multiple paths are given and some are invalid, the valid ones are still verified and their success lines printed; exit code is 1 (partial failure)
- [ ] `ns2 spec verify` on a non-existent file exits non-zero with an error message
- [ ] `ns2 spec verify` on a file without valid frontmatter exits non-zero
- [ ] No server required

## Cleanup

Do not run any cleanup commands. The smoke-test skill tears down containers after all flows complete and may inspect state first.