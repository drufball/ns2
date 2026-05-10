#!/usr/bin/env bash
set -euo pipefail
cd /tmp/ns2-smoke
ns2 agent new \
  --name swe \
  --description "Software engineer agent" \
  --body "You are a software engineer. When asked to do something, do it concisely and confirm completion. When you are done, call the stop tool with status='complete' and a brief comment summarizing what you did."
