# CLAUDE.md

## Project

A CLI coding agent + orchestration framework. Core architecture is a localhost HTTP server with SSE; the TUI attaches to sessions as a thin client.

## Stack

- **Language:** Rust (Cargo workspace — every module is its own crate)
- **Server:** axum (localhost HTTP + SSE)
- **Database:** SQLite via sqlx (`#[sqlx::test]` for per-test in-memory DBs)
- **TUI:** ratatui, thin client over SSE

## Architecture

See @architecture.spec.md for the full crate structure and design philosophy.

**Verification loops are paramount:** unit tests, integration tests, compile, and clippy must pass before any change is considered done. All new features *MUST* be testable via unit tests and manual testing by coding agents.

## Commands

```bash
cargo check                        # fast compile check (no output)
cargo build                        # build all crates
cargo clippy -- -D warnings        # lint — warnings are errors
cargo test                         # run all tests
cargo test -p <crate>              # test a single crate
cargo test <name>                  # run tests matching a name pattern
cargo test -p <crate> <name>       # both
```

The verification loop before any change is considered done: `cargo clippy -- -D warnings && cargo test`.

## Testing

**Red-green TDD.** Write a failing test first, then make it pass. When debugging, start by writing a test that reproduces the reported error before touching any code.

**Test only the public API.** Use `mod tests {}` for locality, but only call `pub` functions and use `pub` types — never access private internals even though Rust permits it. If something is hard to test through the public API, the API needs redesigning, not the test.

**Mock at crate boundaries.** Each crate should define traits for any dependencies it takes from other crates. In tests, mock those traits using `mockall` rather than pulling in the real dependency. This keeps each crate's tests fast, isolated, and free of infrastructure (no real DB, no real HTTP, no real filesystem unless you're explicitly testing that layer). The real implementations are exercised in integration tests at the crate that wires everything together.
