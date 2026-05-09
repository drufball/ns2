---
targets:
  - crates/cli/src/commands/spec.rs
verified: 2026-05-09T06:32:58Z
---

# ns2 spec

Specs are Markdown files that describe the intended behavior of a part of the codebase and declare which source files they govern. They serve two purposes: human-readable design documentation for understanding the implementation, and a CI staleness check that fails when covered source files change without the spec being reviewed.

## Frontmatter

Every spec file has YAML frontmatter with two required fields:

- `targets` — glob patterns (relative to git root) for the source files this spec covers
- `verified` — UTC timestamp of the last review; absent means the spec has never been verified

An optional `severity` field (default: `error`) can be set to `warning` for aspirational or in-progress specs that shouldn't fail CI hard.

## Lifecycle

- **unverified** — spec was just created or has never been reviewed
- **stale** — at least one target file was modified after the `verified` timestamp
- **clean** — all target files are older than `verified`

## Key commands

`ns2 spec new <path>` scaffolds a new spec with a `--target` glob and no verified timestamp. The body is left empty for you to fill in.

`ns2 spec verify <path>` writes the current UTC timestamp into the `verified` field. Run this after reviewing a spec and confirming it still matches the code.

`ns2 spec sync` checks every spec in the repo and prints an error for each stale one, then exits non-zero if any error-severity spec is stale. Pass `--error-on-warnings` to treat warning-severity specs as errors too — useful for strict CI gates. You can also pass a specific file or directory to limit the check.

## CI integration

Add `ns2 spec sync --error-on-warnings` to your CI pipeline to enforce that specs stay in sync with the code they describe.