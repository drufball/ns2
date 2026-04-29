---
name: architecture-review
description: Architecture review covering spec compliance, automated checks, independent file analysis, and suggestions for improving the checks themselves. Use when asked to review the codebase or check for architectural violations.
---

Review the changed files (or full codebase if no branch context). Run these checks in order and report findings by section:

- **Spec compliance** — read `crates/arch-tests/architecture.spec.md`, then run `cargo test -p arch-tests`. Flag any violations against the spec or failing tests.
- **Automated checks** — build first with `cargo build -p arch-tests -q`, then for each changed `.rs` file run:
  - `target/debug/check-loc` (threshold 1000 code LOC)
  - `target/debug/check-fanout` (flag score > 300)
  - `target/debug/check-state` (flag score > 8)
  - `target/debug/cohesion <file>` (flag concern score > 12; include cluster output for flagged files)
- **Independent review** — read each changed file and flag issues the automated checks missed: mixed concerns, unclear boundaries, assumption gaps at callsites.
- **Check improvement suggestions** — for anything caught in independent review, note whether a tweak to one of the four scripts (threshold, pattern, formula) would have caught it automatically, and whether any current flags look like false positives worth suppressing.
