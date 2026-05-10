#!/usr/bin/env bash
set -euo pipefail
# Copies the host .env (mounted at /tmp/ns2-host.env) into the repo root
# so ns2 auto-loads ANTHROPIC_API_KEY on every invocation.
cp /tmp/ns2-host.env /tmp/ns2-smoke/.env
