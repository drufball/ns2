---
targets:
  - product-flows/fixtures/*.sh
verified: 2026-04-24T12:32:23Z
---


# Fixtures

Composable setup scripts mounted at `/fixtures/` in every test container. Each fixture does one thing and makes no assumptions about what else is present. Flows compose fixtures by calling multiple scripts in sequence — start with `init.sh`, then layer only what the flow actually needs. Avoid adding to an existing fixture when a new one would do; a fixture that mixes unrelated setup becomes a hidden dependency for every flow that uses it.

## `init.sh`
Bare git repo at `/repo` with a single commit.
```
/repo/
└── README.md
```

## `start-server.sh`
Copies `.env` from the host mount and starts the ns2 server in the background.

## `seeded-files.sh`
Adds files with known static content for agent read-back testing.
```
/repo/
├── read-test.txt        ("The secret value is: ns2-read-tool-test-42")
└── multi-turn-test.txt  ("The magic number is: 7742")
```

## `codebase-layout.sh`
Adds a minimal Rust codebase structure and a spec file without targets frontmatter.
```
/repo/
└── crates/
    ├── agents/src/lib.rs
    ├── arch-tests/architecture.spec.md   ← no targets frontmatter
    └── cli/src/main.rs
```