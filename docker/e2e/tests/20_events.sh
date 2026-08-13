#!/usr/bin/env bash
# 20 — andler events: daemon event stream over gRPC (lifecycle + QMP).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/20-events"
mkdir -p "$WORK"

echo "  [lifecycle events arrive on the stream]"
# Subscribe first (non-follow: exits after the first event), then create.
( sleep 1; create_linux "$WORK/li" e2e-events >/dev/null ) &
expect_ok "events stream delivers" -- timeout 30 andler events
expect_out_grep "instance created event" "instance created"

ID="$(andler list --json | sed -n 's/.*"id": *"\([a-f0-9]\{8\}\).*/\1/p' | head -1)"
[[ -n "$ID" ]] || fail "empty instance id"

echo "  [instance filter narrows the stream]"
( sleep 1; andler start "$ID" >/dev/null ) &
expect_ok "events with a filter resolves the prefix" -- timeout 60 andler events "$ID" --json
expect_out_grep "json kind field" '"kind":"Lifecycle"'
expect_out_grep "filtered instance id" "$ID"

echo "  [stop produces lifecycle events on the filtered stream]"
( sleep 1; andler stop "$ID" --graceful >/dev/null ) &
expect_ok "stop events" -- timeout 60 andler events "$ID" --json
expect_out_grep "stopped transition" '"kind":"Lifecycle"'

echo "  [cleanup]"
# The background stop is still settling when the stream exits on the first
# event; wait for the terminal state before removing.
for _ in $(seq 1 30); do
    if andler status "$ID" 2>/dev/null | grep -q "Stopped"; then break; fi
    sleep 1
done
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
