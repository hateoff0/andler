#!/usr/bin/env bash
# 09 — daemon restart persistence and final cleanup. Must run last: it
# restarts the shared daemon process.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/09-persistence"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-persist)"
[[ -n "$ID" ]] || fail "empty id from create"

expect_ok "start" -- andler start "$ID"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "status shows Stopped" -- andler status "$ID"
expect_out_grep "Stopped" "Stopped"

echo "  [daemon restart]"
ID_CREATED="$(create_linux "$WORK" e2e-persist-created)"
[[ -n "$ID_CREATED" ]] || fail "empty id from create"
stop_daemon
start_daemon

expect_ok "instance survives the daemon restart" -- andler list
expect_out_grep "persisted id" "$ID"
expect_out_grep "persisted state is Stopped" "Stopped"
expect_out_grep "created-only instance survives" "$ID_CREATED"

expect_ok "status after the restart" -- andler status "$ID"
expect_out_grep "status Stopped" "Stopped"

expect_ok "remove --purge" -- andler remove "$ID_CREATED" --purge

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"