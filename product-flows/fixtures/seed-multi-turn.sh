#!/usr/bin/env bash
set -euo pipefail
echo "The magic number is: 7742" > /tmp/ns2-smoke/multi-turn-test.txt
git -C /tmp/ns2-smoke add .
git -C /tmp/ns2-smoke commit -m "seed"
