#!/usr/bin/env bash
set -euo pipefail
cp /tmp/ns2-host.env /repo/.env
cd /repo
ns2 server start &
sleep 1
