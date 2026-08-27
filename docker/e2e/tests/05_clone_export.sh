#!/usr/bin/env bash
# 05 — clone & export: all three clone modes, live-clone removal protection,
# export, and their error paths (AndroidVm only).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/05-clone"
mkdir -p "$WORK"

ANDROID_BASE="$WORK/android-base.qcow2"
ANDROID_ROOT="$WORK/android-instances"
qemu-img create -f qcow2 "$ANDROID_BASE" 4G >/dev/null 2>&1
qemu-img create -f raw "$WORK/OVMF_VARS.template.fd" 4M >/dev/null 2>&1

echo "  [android create]"
expect_fail "create android with a missing base image" -- andler create --kind android --name bad-base --android-version 13 --base-image-path "$WORK/missing.qcow2" --instances-root "$ANDROID_ROOT" --ovmf-vars-template "$WORK/OVMF_VARS.template.fd"
expect_err_grep "missing base image error is actionable" "base image not found"

SRC="$(create_android "$WORK" source-android "$ANDROID_BASE")"
[[ -n "$SRC" ]] || fail "empty id from android create"
pass "android create returns an instance id"

echo "  [clone modes]"
LINKED="$(andler clone "$SRC" --name linked-clone --instances-root "$ANDROID_ROOT" --mode linked | sed -n 's/^cloned instance_id=//p')"
[[ -n "$LINKED" ]] || fail "empty id from clone --mode linked"
pass "clone --mode linked"
    expect_ok "clone --json" -- andler clone "$SRC" --name linked-clone-json --instances-root "$ANDROID_ROOT" --mode linked --json
    expect_ok "clone --json reports source_instance_id" -- jq -e '.source_instance_id == "'"$SRC"'"' <<<"$(cat "$E2E_LAST_OUT")"
    expect_ok "clone --json reports instance_id" -- jq -e '.instance_id | test("^[0-9a-f]{64}$")' <<<"$(cat "$E2E_LAST_OUT")"

expect_fail "remove --purge on source with a live linked clone" -- andler remove "$SRC" --purge
expect_err_grep "live-clone protection message" "live"

STANDALONE="$(andler clone "$SRC" --name standalone-clone --instances-root "$ANDROID_ROOT" --mode full-standalone | sed -n 's/^cloned instance_id=//p')"
[[ -n "$STANDALONE" ]] || fail "empty id from clone --mode full-standalone"
pass "clone --mode full-standalone"

SHARED="$(andler clone "$SRC" --name shared-base-clone --instances-root "$ANDROID_ROOT" --mode shared-base | sed -n 's/^cloned instance_id=//p')"
[[ -n "$SHARED" ]] || fail "empty id from clone --mode shared-base"
pass "clone --mode shared-base"

SHARED_DIR="$(ls -d "$ANDROID_ROOT"/"$SHARED"* 2>/dev/null | head -1)"
[[ -n "$SHARED_DIR" && -e "$SHARED_DIR/disk.qcow2" ]] || fail "shared-base clone disk file missing"
pass "shared-base clone has a disk file"

echo "  [disk copy semantics]"
SRC_DIR="$(ls -d "$ANDROID_ROOT"/"$SRC"* 2>/dev/null | head -1)"
LINKED_DIR="$(ls -d "$ANDROID_ROOT"/"$LINKED"* 2>/dev/null | head -1)"
STANDALONE_DIR="$(ls -d "$ANDROID_ROOT"/"$STANDALONE"* 2>/dev/null | head -1)"
[[ -n "$SRC_DIR" && -n "$LINKED_DIR" && -n "$STANDALONE_DIR" ]] || fail "instance dirs missing"

STANDALONE_INFO="$(qemu-img info --output=json "$STANDALONE_DIR/disk.qcow2")"
# qemu-img omits "backing-filename" entirely (rather than emitting null)
# when there is no backing file — absence is the "real full copy" signal.
if ! grep -q '"backing-filename":' <<<"$STANDALONE_INFO"; then
    pass "full-standalone disk is a real full copy (no backing file)"
else
    echo "$STANDALONE_INFO"
    fail "full-standalone disk unexpectedly has a backing file (overlay?)"
fi

LINKED_BACKING="$(qemu-img info --output=json "$LINKED_DIR/disk.qcow2" | grep -o '"backing-filename": "[^"]*"' | cut -d'"' -f4)"
if [[ "$LINKED_BACKING" == "$SRC_DIR/disk.qcow2" ]]; then
    pass "linked clone overlays the source disk"
else
    fail "linked clone backing is ${LINKED_BACKING:-<none>}, expected $SRC_DIR/disk.qcow2"
fi

SHARED_BACKING="$(qemu-img info --output=json "$SHARED_DIR/disk.qcow2" | grep -o '"backing-filename": "[^"]*"' | cut -d'"' -f4)"
if [[ "$SHARED_BACKING" == "$ANDROID_BASE" ]]; then
    pass "shared-base clone overlays the base image"
else
    fail "shared-base clone backing is ${SHARED_BACKING:-<none>}, expected $ANDROID_BASE"
fi

expect_fail "clone of a nonexistent source" -- andler clone "$UNKNOWN_ID" --name x --instances-root "$ANDROID_ROOT" --mode linked

echo "  [export]"
expect_ok "export to a standalone file" -- andler export "$SRC" "$WORK/exported-android-disk.qcow2"
expect_file "exported disk exists" "$WORK/exported-android-disk.qcow2"
    expect_ok "export --json" -- andler export "$SRC" "$WORK/exported-android-disk.qcow2" --json
    expect_ok "export --json reports dest_path" -- jq -e '.dest_path == "'"$WORK/exported-android-disk.qcow2"'"' <<<"$(cat "$E2E_LAST_OUT")"
    expect_ok "export --json reports source_instance_id" -- jq -e '.source_instance_id == "'"$SRC"'"' <<<"$(cat "$E2E_LAST_OUT")"

expect_fail "export of a nonexistent source" -- andler export "$UNKNOWN_ID" "$WORK/x.qcow2"

LIST_AFTER_EXPORT="$(andler list)"
LIST_AFTER_EXPORT_COUNT="$(wc -l <<<"$LIST_AFTER_EXPORT")"
if [[ "$LIST_AFTER_EXPORT_COUNT" -eq 4 ]]; then
    pass "export registers no new instance (4 instances)"
else
    echo "$LIST_AFTER_EXPORT"
    fail "expected 4 instances after export, got $LIST_AFTER_EXPORT_COUNT"
fi

echo "  [cleanup]"
expect_ok "remove standalone clone" -- andler remove "$STANDALONE" --purge
expect_ok "remove shared-base clone" -- andler remove "$SHARED" --purge
expect_ok "remove linked clone" -- andler remove "$LINKED" --purge
expect_ok "remove source after clones are gone" -- andler remove "$SRC" --purge

expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
