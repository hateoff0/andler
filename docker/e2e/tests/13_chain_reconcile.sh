#!/usr/bin/env bash
# 13 — snapshot chain reconciliation across daemon restarts: clone
# protection, crash-mid-restore rebuild, tmp-overlay promotion, orphaned
# layer recovery, branch switch-back. Restarts the shared daemon.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/13-chain-reconcile"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-chain)"
[[ -n "$ID" ]] || fail "empty instance id from create"
INSTANCE_DIR="$(echo "$HOME/.andler/instances/$ID"*/)"
INSTANCE_DIR="${INSTANCE_DIR%/}"
SNAP_DIR="$INSTANCE_DIR/disk.snapshots"

gen_uuid() { cat /proc/sys/kernel/random/uuid; }

echo "  [chain setup]"
expect_ok "start" -- andler start "$ID"
expect_ok "snapshot create snap-1" -- andler snapshot create "$ID" --tag snap-1
expect_ok "snapshot create snap-2" -- andler snapshot create "$ID" --tag snap-2
expect_ok "stop" -- andler stop "$ID" --graceful
LAYER_COUNT="$(ls "$SNAP_DIR"/*.qcow2 2>/dev/null | wc -l)"
[[ "$LAYER_COUNT" -eq 2 ]] || fail "expected two layers, found $LAYER_COUNT"
pass "two external layers on disk"

echo "  [clone protection]"
CLONE_ID="$(andler clone "$ID" --name chain-clone --mode linked | sed -n 's/^cloned instance_id=//p')"
[[ -n "$CLONE_ID" ]] || fail "empty clone id"
# the clone derives from the active disk, whose backing chain reaches the
# newest layer; restore/discard or deleting that layer would orphan it
expect_fail "restore blocked while a clone derives from the chain" -- andler snapshot restore "$ID" --tag snap-1
expect_err_grep "restore-while-cloned message" "remove the clones first"
expect_fail "layer delete blocked while a clone consumes it" -- andler snapshot delete "$ID" --tag snap-2
expect_err_grep "delete-while-cloned message" "remove the clones first"

expect_ok "remove clone" -- andler remove "$CLONE_ID" --purge
expect_ok "restore allowed after clone removal" -- andler snapshot restore "$ID" --tag snap-1
# the discard restore deleted snap-2 (newer than the target); recreate it so
# the crash scenarios below start from a two-layer chain again
expect_ok "start" -- andler start "$ID"
expect_ok "snapshot create snap-2 (again)" -- andler snapshot create "$ID" --tag snap-2
expect_ok "stop" -- andler stop "$ID" --graceful

echo "  [crash mid-restore: active disk rebuilt on chain head]"
# Simulate a crash between "delete old active disk" and "create new overlay":
# the active file is gone, layers untouched. Startup reconciliation must
# rebuild disk.qcow2 on top of the newest layer (snap-2).
rm -f "$WORK/disk.qcow2"
stop_daemon
start_daemon

expect_ok "rebuilt active disk appears after restart" -- qemu-img info "$WORK/disk.qcow2" >/dev/null
BACKING="$(qemu-img info --output=json "$WORK/disk.qcow2" | jq -r '."backing-filename" // empty')"
[[ "$BACKING" == "$SNAP_DIR"/*.qcow2 ]] || fail "rebuilt disk does not back onto a layer: $BACKING"
pass "rebuilt active disk backs onto the chain head"

expect_ok "snapshots survived the crash" -- andler snapshot list "$ID"
expect_out_grep "snap-1 listed" "tag=snap-1"
expect_out_grep "snap-2 listed" "tag=snap-2"

echo "  [crash between the create rename pair: tmp overlay promoted]"
# A live-snapshot create crashed after "old active -> layer" but before
# ".tmp-* -> active". Replay the state by hand: the previous active file
# becomes a layer, a staging overlay backs onto it, no active disk exists.
PROMOTED_ID="$(gen_uuid)"
PROMOTED_LAYER="$SNAP_DIR/$PROMOTED_ID.qcow2"
mv "$WORK/disk.qcow2" "$PROMOTED_LAYER"
TMP_OVERLAY="$SNAP_DIR/.tmp-$PROMOTED_ID.qcow2"
qemu-img create -f qcow2 -b "$PROMOTED_LAYER" -F qcow2 "$TMP_OVERLAY" >/dev/null 2>&1
stop_daemon
start_daemon

expect_ok "promoted active disk appears after restart" -- qemu-img info "$WORK/disk.qcow2" >/dev/null
PROMOTED_BACKING="$(qemu-img info --output=json "$WORK/disk.qcow2" | jq -r '."backing-filename" // empty')"
[[ "$PROMOTED_BACKING" == "$PROMOTED_LAYER" ]] || fail "promoted disk backs onto the interrupted layer, got: $PROMOTED_BACKING"
pass "promoted active disk backs onto the interrupted layer"
expect_no_file "staging overlay consumed by promotion" "$TMP_OVERLAY"
expect_ok "promoted layer recovered in the store" -- andler snapshot list "$ID"
expect_out_grep "interrupted layer got a recovered entry" "tag=recovered-${PROMOTED_ID:0:8}"

echo "  [orphaned tmp overlay is removed]"
TMP_ORPHAN="$SNAP_DIR/.tmp-deadbeef.qcow2"
touch "$TMP_ORPHAN"
stop_daemon
start_daemon
expect_no_file "orphaned tmp overlay removed" "$TMP_ORPHAN"

echo "  [crash after the rename pair, before the INSERT: layer recovered]"
# The rename pair landed (old active -> layer, staging -> active) but the
# store INSERT never happened; the layer has no metadata row.
NEW_ID="$(gen_uuid)"
mv "$WORK/disk.qcow2" "$SNAP_DIR/$NEW_ID.qcow2"
qemu-img create -f qcow2 -b "$SNAP_DIR/$NEW_ID.qcow2" -F qcow2 "$WORK/disk.qcow2" >/dev/null 2>&1
stop_daemon
start_daemon
expect_ok "recovered layer listed" -- andler snapshot list "$ID"
expect_out_grep "recovered entry created" "tag=recovered-${NEW_ID:0:8}"
expect_file "recovered layer file present" "$SNAP_DIR/$NEW_ID.qcow2"

echo "  [metadata entry without a layer file is dropped]"
# An archived branch head deleted behind the daemon's back: its entry must
# be dropped while the live chain (which does not reach it) stays intact.
expect_ok "branch restore of snap-1" -- andler snapshot restore "$ID" --tag snap-1 --branch
PRE_ID="$(andler snapshot --json list "$ID" | jq -r '.[] | select(.tag | startswith("pre-branch-")) | .snapshot_id')"
[[ -n "$PRE_ID" ]] || fail "no archived pre-branch-* record found"
ARCHIVED_HEAD="$SNAP_DIR/$PRE_ID.qcow2"
[[ -f "$ARCHIVED_HEAD" ]] || fail "archived head file missing: $ARCHIVED_HEAD"
rm -f "$ARCHIVED_HEAD"
stop_daemon
start_daemon
expect_ok "list after archived head removal" -- andler snapshot list "$ID"
expect_out_nogrep "archived head entry dropped" "tag=pre-branch-"

echo "  [branch switch-back returns ancestors to the main branch]"
expect_ok "branch restore of snap-2" -- andler snapshot restore "$ID" --tag snap-2 --branch
expect_ok "list after switch-back" -- andler snapshot list "$ID"
expect_ok "snap-1 back on the main branch" -- jq -e '.[] | select(.tag == "snap-1") | .branch == ""' <<<"$(andler snapshot --json list "$ID")"
expect_ok "snap-2 back on the main branch" -- jq -e '.[] | select(.tag == "snap-2") | .branch == ""' <<<"$(andler snapshot --json list "$ID")"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
