# CLAUDE.md

ns2 is a CLI coding agent + orchestration framework built with Rust. See specs/architecture.spec.md for full crate structure and design philosophy.

## Commands

```bash
cargo check
cargo build
cargo clippy --tests -- -D warnings -W clippy::pedantic -W clippy::nursery
cargo test
cargo llvm-cov --summary-only
```

## Sub tasks and agents

- Break work into sub tasks and use sequential subagents to complete each sub task
- Always run subagents one at a time
- Never use parallel or background subagents
- Complete sub tasks on the same branch/worktree 

## Finishing work

Before stopping, always commit all changes and push to the remote branch. Do not create a PR unless explicitly asked, but always commit and push so that your work is not lost.

## Testing

- Use red-green TDD for all development.
- When debugging, create a test that reproduces the reported error before touching any code.
- Unit tests must mock all traits imported from other crates.
