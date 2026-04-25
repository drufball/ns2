---
targets:
  - crates/specs/src/**/*.rs
  - crates/cli/src/main.rs
severity: warning
verified: 2026-04-25T10:03:20Z
---

# Flow 10: Spec New

Create new `.spec.md` files using `ns2 spec new`. These files declare which source files a
spec governs via glob patterns in the `targets` frontmatter field.

These commands operate purely on the filesystem — no server required.

## Prerequisites

No API key required. No server needed.

## Fixture Setup

```bash
docker exec ns2-flow-10 bash /fixtures/init.sh
```

The server is intentionally not started — spec commands are filesystem-only.

## Steps

### Create a spec file with one target

```bash
docker exec ns2-flow-10 bash -c 'cd /repo && ns2 spec new crates/cli/cli-commands.spec.md --target "crates/cli/src/**/*.rs"'
```

Expected output on stdout:
```
Created spec at crates/cli/cli-commands.spec.md
```

Verify the file was written correctly:

```bash
docker exec ns2-flow-10 bash -c 'cat /repo/crates/cli/cli-commands.spec.md'
```

Expected file contents:
```
---
targets:
  - crates/cli/src/**/*.rs
---
```

The `verified` field is absent because the spec has not been verified yet.

### Create a spec file with multiple targets

```bash
docker exec ns2-flow-10 bash -c 'cd /repo && ns2 spec new crates/agents/agents.spec.md --target "crates/agents/src/**/*.rs" --target "crates/agents/Cargo.toml"'
```

Expected output on stdout:
```
Created spec at crates/agents/agents.spec.md
```

Verify:

```bash
docker exec ns2-flow-10 bash -c 'cat /repo/crates/agents/agents.spec.md'
```

Expected file contents:
```
---
targets:
  - crates/agents/src/**/*.rs
  - crates/agents/Cargo.toml
---
```

Both targets appear in the `targets` list.

## Error Cases

### `spec new` on a path that already exists

```bash
docker exec ns2-flow-10 bash -c 'cd /repo && ns2 spec new crates/cli/cli-commands.spec.md --target "crates/cli/src/**/*.rs"; echo "Exit code: $?"'
```

Expected: error message on stderr and `Exit code: 1` (or any non-zero value). The existing file is unchanged.

### `spec new` without `--target`

```bash
docker exec ns2-flow-10 bash -c 'cd /repo && ns2 spec new crates/foo/bar.spec.md; echo "Exit code: $?"'
```

Expected: error message on stderr and `Exit code: 1`. At least one `--target` flag is required.

## Acceptance Criteria

- [ ] `ns2 spec new <path> --target <glob>` creates the file with a `targets` list in frontmatter
- [ ] File has no `verified` field when newly created
- [ ] `ns2 spec new` with multiple `--target` flags writes all patterns into the `targets` list
- [ ] `ns2 spec new` prints `Created spec at <path>` on stdout and exits 0
- [ ] `ns2 spec new` on an existing path exits non-zero with an error message and does not overwrite
- [ ] `ns2 spec new` without `--target` exits non-zero with an error message
- [ ] No server required