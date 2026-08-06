#!/usr/bin/env bash
# 04 — snapshots: live create/list/delete over QMP, offline restore via
# qemu-img, state-machine negatives.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/04-snapshot"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-snapshot)"
[[ -n "$ID" ]] || fail "empty instance id from create"

echo "  [snapshots while running]"
expect_ok "start" -- andler start "$ID"

expect_fail "remove while running" -- andler remove "$ID"
expect_err_grep "remove-while-running message" "stop it first"
expect_fail "remove --purge while running" -- andler remove "$ID" --purge

expect_fail "config set display.resolution while running (no guest agent)" -- andler config set "$ID" display.resolution 1920x1080

expect_fail "snapshot restore while running" -- andler snapshot restore "$ID" --tag snap-first
expect_err_grep "restore-while-running message" "must be stopped"

expect_ok "snapshot create snap-first" -- andler snapshot create "$ID" --tag snap-first --description first
expect_out_grep "create reports the tag" "snapshot created: tag=snap-first"
expect_ok "snapshot create snap-second" -- andler snapshot create "$ID" --tag snap-second
expect_fail "snapshot duplicate tag is rejected" -- andler snapshot create "$ID" --tag snap-first

expect_ok "snapshot list" -- andler snapshot list "$ID"
expect_out_grep "lists snap-first" "tag=snap-first"
expect_out_grep "lists snap-second" "tag=snap-second"
expect_out_grep "lists the description" "description=first"

expect_ok "snapshot list --json" -- andler snapshot --json list "$ID"
expect_ok "snapshot list json parses to two" -- jq -e 'length == 2' "$E2E_LAST_OUT"

echo "  [offline restore]"
expect_ok "stop" -- andler stop "$ID" --graceful

expect_ok "snapshot restore while stopped" -- andler snapshot restore "$ID" --tag snap-first
expect_out_grep "restore reports success" "snapshot snap-first restored"

expect_ok "snapshot list after restore" -- andler snapshot list "$ID"
expect_out_grep "snapshots survive an offline restore" "tag=snap-first"

expect_fail "snapshot restore of a missing tag" -- andler snapshot restore "$ID" --tag nope

expect_fail "snapshot create while stopped" -- andler snapshot create "$ID" --tag snap-three
expect_err_grep "create-while-stopped message" "requires running"

expect_fail "snapshot delete while stopped" -- andler snapshot delete "$ID" --tag snap-first
expect_err_grep "delete-while-stopped message" "requires running"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
