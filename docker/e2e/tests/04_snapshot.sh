#!/usr/bin/env bash
# 04 — external snapshots: live create over QMP + rename pair, offline
# restore (discard) and delete (commit + prune), state-machine negatives.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/04-snapshot"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-snapshot)"
[[ -n "$ID" ]] || fail "empty instance id from create"

INSTANCE_DIR="$(echo "$HOME/.andler/instances/$ID"*/)"
INSTANCE_DIR="${INSTANCE_DIR%/}"
SNAP_DIR="$INSTANCE_DIR/disk.snapshots"

echo "  [snapshots while running]"
expect_ok "start" -- andler start "$ID"

expect_fail "remove while running" -- andler remove "$ID"
expect_err_grep "remove-while-running message" "stop it first"
expect_fail "remove --purge while running" -- andler remove "$ID" --purge

expect_fail "snapshot restore while running" -- andler snapshot restore "$ID" --tag snap-first
expect_err_grep "restore-while-running message" "must be stopped"

expect_fail "snapshot delete while running" -- andler snapshot delete "$ID" --tag snap-first
expect_err_grep "delete-while-running message" "must be stopped"

expect_ok "snapshot create snap-first" -- andler snapshot create "$ID" --tag snap-first --description first
expect_out_grep "create reports the tag" "snapshot created: tag=snap-first"
expect_ok "snapshot create snap-second" -- andler snapshot create "$ID" --tag snap-second
expect_fail "snapshot duplicate tag is rejected" -- andler snapshot create "$ID" --tag snap-first

expect_ok "snapshot list" -- andler snapshot list "$ID"
expect_out_grep "lists snap-first" "tag=snap-first"
expect_out_grep "lists snap-second" "tag=snap-second"
expect_out_grep "lists the description" "description=first"

expect_ok "snapshot list --json" -- andler snapshot --json list "$ID"
expect_ok "snapshot list json parses to two" -- jq -e 'length == 2' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [external layer files]"
expect_file "layer file for snap-first exists" "$SNAP_DIR"/*.qcow2
LAYER_COUNT="$(ls "$SNAP_DIR"/*.qcow2 2>/dev/null | wc -l)"
[[ "$LAYER_COUNT" -eq 2 ]] || fail "expected two layer files, found $LAYER_COUNT"
pass "exactly two layer files on disk"

expect_ok "stop" -- andler stop "$ID" --graceful

echo "  [offline restore (discard)]"
expect_ok "snapshot restore while stopped" -- andler snapshot restore "$ID" --tag snap-first
expect_out_grep "restore reports success" "snapshot snap-first restored"

# discard restore deleted the newer layer (snap-second) and its record
[[ -d "$SNAP_DIR" ]] || fail "snapshot dir vanished"
LAYER_COUNT="$(ls "$SNAP_DIR"/*.qcow2 2>/dev/null | wc -l)"
[[ "$LAYER_COUNT" -eq 1 ]] || fail "expected one layer after discard restore, found $LAYER_COUNT"
pass "discard restore removed the newer layer file"
expect_ok "snapshot list after restore" -- andler snapshot list "$ID"
expect_out_grep "snapshots survive an offline restore" "tag=snap-first"
expect_out_nogrep "discarded snapshot is gone from the list" "tag=snap-second"

expect_fail "snapshot restore of a missing tag" -- andler snapshot restore "$ID" --tag nope

echo "  [offline delete (commit + prune)]"
expect_fail "snapshot create while stopped" -- andler snapshot create "$ID" --tag snap-three
expect_err_grep "create-while-stopped message" "requires running"

expect_ok "start" -- andler start "$ID"
expect_ok "snapshot create snap-second (again)" -- andler snapshot create "$ID" --tag snap-second
expect_ok "stop" -- andler stop "$ID" --graceful

# snap-second derives from snap-first; deleting it commits into snap-first
# and re-points the active disk at snap-first.
expect_ok "snapshot delete while stopped" -- andler snapshot delete "$ID" --tag snap-second
expect_out_grep "delete reports success" "snapshot snap-second deleted"
expect_ok "list without the deleted snapshot" -- andler snapshot list "$ID"
expect_out_grep "snap-first still listed" "tag=snap-first"
expect_out_nogrep "deleted snapshot gone from the list" "tag=snap-second"
LAYER_COUNT="$(ls "$SNAP_DIR"/*.qcow2 2>/dev/null | wc -l)"
[[ "$LAYER_COUNT" -eq 1 ]] || fail "expected one layer (snap-first) after delete, found $LAYER_COUNT"
pass "deleted layer file removed, parent layer kept"
expect_ok "active disk still boots metadata-wise" -- qemu-img info "$WORK/disk.qcow2" >/dev/null
ACTIVE_BACKING="$(qemu-img info --output=json "$WORK/disk.qcow2" | jq -r '."backing-filename" // empty')"
[[ "$ACTIVE_BACKING" == "$SNAP_DIR"/*.qcow2 ]] || fail "active disk not re-pointed at the parent layer: $ACTIVE_BACKING"
pass "active disk re-pointed at the parent layer after delete"

# The base layer has no parent to merge into — deletion is refused.
expect_fail "base layer delete is refused" -- andler snapshot delete "$ID" --tag snap-first
expect_err_grep "base-layer message" "no backing"

echo "  [branch restore]"
expect_ok "start" -- andler start "$ID"
expect_ok "snapshot create snap-a" -- andler snapshot create "$ID" --tag snap-a
expect_ok "snapshot create snap-b" -- andler snapshot create "$ID" --tag snap-b
expect_ok "stop" -- andler stop "$ID" --graceful

expect_ok "branch restore of snap-a" -- andler snapshot restore "$ID" --tag snap-a --branch
expect_out_grep "branch restore reports success" "snapshot snap-a restored"

expect_ok "list shows the archived branch" -- andler snapshot list "$ID"
expect_out_grep "archived head pre-*" "tag=pre-branch-"
expect_out_grep "branch column for archived layers" "branch=branch-"
expect_out_grep "snap-a still listed" "tag=snap-a"
expect_out_grep "snap-b still listed" "tag=snap-b"

# snap-first + snap-a + snap-b + pre-branch head all survive a branch restore
LAYER_COUNT="$(ls "$SNAP_DIR"/*.qcow2 2>/dev/null | wc -l)"
[[ "$LAYER_COUNT" -eq 4 ]] || fail "expected four layer files after branch restore, found $LAYER_COUNT"
pass "branch restore keeps all layer files"

# switching back to the archived branch continues from snap-b, and the
# current main chain (snap-a's child) gets archived in its turn
expect_ok "branch restore of snap-b" -- andler snapshot restore "$ID" --tag snap-b --branch
expect_ok "list after switching branches" -- andler snapshot list "$ID"
expect_out_grep "second archived head" "tag=pre-branch-"
expect_out_grep "snap-b on the main branch again" "tag=snap-b"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
