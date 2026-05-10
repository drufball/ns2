#!/usr/bin/env bash
set -euo pipefail
mkdir -p /tmp/ns2-smoke
git -C /tmp/ns2-smoke init
git -C /tmp/ns2-smoke commit --allow-empty -m "init"
