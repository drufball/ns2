#!/usr/bin/env bash
# Adds a realistic codebase layout to /repo: source files for spec targets to track,
# and a plain .spec.md without frontmatter to exercise the "silently skipped" path.
set -euo pipefail

mkdir -p /repo/crates/cli/src
echo 'fn main() {}' > /repo/crates/cli/src/main.rs

mkdir -p /repo/crates/agents/src
echo 'pub fn hello() {}' > /repo/crates/agents/src/lib.rs

mkdir -p /repo/crates/arch-tests
printf '# Architecture Specification\n\nPlain doc without targets frontmatter.\n' \
    > /repo/crates/arch-tests/architecture.spec.md

git -C /repo add -A
git -C /repo commit -m "add codebase layout"
