# CLAUDE.md

ns2 is a CLI coding agent + orchestration framework built with Rust. See crates/arch-tests/architecture.spec.md for full crate structure and design philosophy.

## Commands

```bash
cargo check
cargo build
cargo clippy -- -D warnings
cargo test
cargo llvm-cov --summary-only
```

## Testing

- Use red-green TDD for all development.
- When debugging, create a test that reproduces the reported error before touching any code.
- Unit tests must mock all traits imported from other crates.
## Dogfooding

You must dogfood ns2 as part of your development workflow. At the start of every session, run `ns2 --help` and `ns2 agent list` for context.

If a listed agent matches a task you need to complete, use `ns2 session new --agent ...`. You may continue using subagents for tasks that do not match a listed agent.

### Critical review

ns2 is development software — do not assume it is free of bugs or UX issues. While using it:

- If output seems wrong or behavior is surprising, check the implementation for bugs.
- Actively note rough edges: confusing help text or error messages, missing features, poor agent ux, bad output quality.

**End every session with an "ns2 improvement suggestions" section** listing anything you observed. Accepted suggestions should be filed as GitHub issues with `gh issue create`.