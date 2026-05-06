# CLAUDE.md

ns2 is a CLI coding agent + orchestration framework built with Rust. See specs/architecture.spec.md for full crate structure and design philosophy.

## Commands

```bash
cargo check
cargo build
cargo clippy -- -D warnings
cargo test
cargo llvm-cov --summary-only
```

## Finishing work

Before stopping, always commit all changes and push to the remote branch. Do not create a PR unless explicitly asked, but always commit and push so that your work is not lost.

## Testing

- Use red-green TDD for all development.
- When debugging, create a test that reproduces the reported error before touching any code.
- Unit tests must mock all traits imported from other crates.