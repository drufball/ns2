#!/usr/bin/env bash
set -euo pipefail
git init /repo
git -C /repo config user.email "test@example.com"
git -C /repo config user.name "ns2 tester"
echo "# ns2-test-repo" > /repo/README.md
git -C /repo add README.md
git -C /repo commit -m "initial commit"
