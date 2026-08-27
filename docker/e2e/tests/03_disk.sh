#!/usr/bin/env bash
# 03 — disk commands: create/info/resize/compact and their error paths
# (pure qemu-img wrappers, no daemon state involved).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/03-disk"
mkdir -p "$WORK"

DISK="$WORK/test.qcow2"

echo "  [disk create/info]"
expect_ok "disk create 1G" -- andler disk create "$DISK" --size 1G
expect_file "disk file exists" "$DISK"

expect_ok "disk info" -- andler disk info "$DISK"
expect_out_grep "info reports qcow2" "format: *qcow2"
expect_out_grep "info reports virtual size" "virtual_size:"
    expect_ok "disk info --json" -- andler disk info "$DISK" --json
    expect_ok "disk info --json reports virtual_size" -- jq -e '.virtual_size == 1073741824' <<<"$(cat "$E2E_LAST_OUT")"
    expect_ok "disk info --json reports backing_file null" -- jq -e '.backing_file == null' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [disk resize]"
expect_ok "disk resize to 2G" -- andler disk resize "$DISK" --size 2G
expect_ok "disk info after resize" -- andler disk info "$DISK"
expect_out_grep "resize took effect" "virtual_size: 2"

expect_fail "disk shrink without --shrink is refused" -- andler disk resize "$DISK" --size 1G
expect_err_grep "shrink refusal is actionable" "refusing to shrink"

echo "  [disk compact]"
expect_ok "disk compact on qcow2" -- andler disk compact "$DISK"
expect_out_grep "compact reports success" "compacted"

RAW="$WORK/plain.raw"
qemu-img create -f raw "$RAW" 64M >/dev/null 2>&1
expect_ok "disk info on raw" -- andler disk info "$RAW"
expect_out_grep "raw format reported" "format: *raw"

expect_ok "disk compact on raw reports not applicable" -- andler disk compact "$RAW"
expect_out_grep "raw compact explains itself" "not applicable"

echo "  [disk negatives]"
expect_fail "disk info of a missing file" -- andler disk info "$WORK/nope.qcow2"
expect_fail "disk create size 0" -- andler disk create "$WORK/zero.qcow2" --size 0
expect_err_grep "zero-size error" "0-byte"
expect_fail "disk create unparsable size" -- andler disk create "$WORK/bad.qcow2" --size banana
