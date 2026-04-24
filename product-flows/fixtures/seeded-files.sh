#!/usr/bin/env bash
# Adds files with known static content for agent read-back testing.
set -euo pipefail
echo "The secret value is: ns2-read-tool-test-42" > /repo/read-test.txt
echo "The magic number is: 7742" > /repo/multi-turn-test.txt
