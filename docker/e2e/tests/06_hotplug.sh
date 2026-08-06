#!/usr/bin/env bash
# 06 — hotplug: attach/detach extra disks and network devices on a live VM,
# config persistence, host-side rollback, and state-machine negatives.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/06-hotplug"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-hotplug)"
[[ -n "$ID" ]] || fail "empty instance id from create"

EXTRA1="$WORK/extra1.qcow2"
EXTRA2="$WORK/extra2.qcow2"

echo "  [state gate]"
expect_fail "attach disk while stopped" -- andler attach disk "$ID" --path "$EXTRA1" --size 2G
expect_err_grep "state gate is actionable" "start it first"
expect_fail "attach net while stopped" -- andler attach net "$ID"
expect_err_grep "net gate too" "start it first"

echo "  [disk attach]"
expect_fail "attach missing path without --size" -- andler attach disk "$ID" --path "$EXTRA1"
expect_err_grep "missing size is explained" "--size is required"

expect_ok "start" -- andler start "$ID"

expect_ok "attach new disk 2G" -- andler attach disk "$ID" --path "$EXTRA1" --size 2G
expect_out_grep "reports the resolved path" "disk attached: $EXTRA1"
expect_out_grep "reports index 0" "extra disk index 0"
expect_file "new disk image exists" "$EXTRA1"
expect_ok "new image is qcow2" -- qemu-img info "$EXTRA1"
expect_out_grep "qcow2 format" "file format: qcow2"

qemu-img create -f qcow2 "$EXTRA2" 4G >/dev/null 2>&1
expect_ok "attach existing image without --size" -- andler attach disk "$ID" --path "$EXTRA2"
expect_out_grep "reports index 1" "extra disk index 1"

expect_fail "attach duplicate path" -- andler attach disk "$ID" --path "$EXTRA1"
expect_err_grep "duplicate rejected" "already attached"
expect_fail "attach primary disk path" -- andler attach disk "$ID" --path "$WORK/disk.qcow2"
expect_err_grep "primary disk protected" "already attached"

echo "  [network attach]"
expect_ok "attach nat network" -- andler attach net "$ID"
expect_out_grep "reports index 0" "extra network index 0"
expect_ok "attach second nat network" -- andler attach net "$ID"
expect_out_grep "reports index 1" "extra network index 1"

expect_fail "attach net on a missing bridge" -- andler attach net "$ID" --mode bridge --bridge no-such-bridge
expect_err_grep "missing bridge is reported" "no-such-bridge"
expect_fail "attach isolated network is unimplemented" -- andler attach net "$ID" --mode isolated
expect_err_grep "isolated explains the alternative" "Use NAT or Bridge mode"

echo "  [config persistence while live]"
expect_ok "config view shows hotplugged devices" -- env VISUAL=cat andler config view "$ID"
expect_out_grep "extra disk 1 listed" "extra1.qcow2"
expect_out_grep "extra disks array present" "\[\[extra_disks\]\]"
expect_out_grep "extra networks array present" "\[\[extra_networks\]\]"

echo "  [detach]"
expect_ok "detach second network by index" -- andler detach net "$ID" 1
expect_out_grep "reports the index" "network detached (extra network index 1)"
expect_fail "detach same index again" -- andler detach net "$ID" 1
expect_err_grep "out-of-range detach rejected" "is not attached"
expect_ok "detach first network" -- andler detach net "$ID" 0

expect_ok "detach disk extra2" -- andler detach disk "$ID" "$EXTRA2"
expect_out_grep "file is kept" "image file was kept"
expect_file "disk image survives detach" "$EXTRA2"
expect_fail "detach unknown path" -- andler detach disk "$ID" "$WORK/nope.qcow2"
expect_err_grep "unknown path rejected" "is not attached"

echo "  [cleanup]"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"