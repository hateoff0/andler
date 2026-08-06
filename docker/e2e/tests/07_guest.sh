#!/usr/bin/env bash
# 07 — guest operations. Error paths run always; the deep section (offline
# package install/remove/list via qemu-nbd, Android boot-mode switching)
# needs the nbd kernel module + CAP_SYS_ADMIN (compose runs privileged) and
# skips cleanly when unavailable.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/07-guest"
mkdir -p "$WORK"

echo "  [guest error paths]"
expect_ok "guest list without an id prints usage" -- andler guest list
expect_out_grep "usage text" "guest list <instance-id>"

LID_ERR="$(create_linux "$WORK/err" e2e-guest-linux)"
[[ -n "$LID_ERR" ]] || fail "empty id from create"

expect_fail "boot-mode on a Linux instance" -- andler guest boot-mode "$LID_ERR" android
expect_err_grep "not-an-android message" "not an Android"

expect_ok "remove error-path instance" -- andler remove "$LID_ERR" --purge

if [[ "${E2E_DEEP_GUEST:-1}" != "1" ]]; then
    echo "  SKIP: deep guest tests disabled (E2E_DEEP_GUEST != 1)"
    exit 0
fi
if ! modprobe nbd max_part=8 2>/dev/null && [[ ! -e /dev/nbd0 ]]; then
    echo "  SKIP: nbd kernel module unavailable"
    exit 0
fi
if [[ ! -e /dev/nbd0 ]]; then
    echo "  SKIP: no /dev/nbd* device (module loaded but no devices)"
    exit 0
fi

echo "  [deep: craft a real guest disk from the baked rootfs]"
GUEST_QCOW="$WORK/guest-disk.qcow2"
qemu-img create -f qcow2 "$GUEST_QCOW" 2G >/dev/null 2>&1

if ! qemu-nbd --connect=/dev/nbd0 "$GUEST_QCOW"; then
    echo "  SKIP: qemu-nbd cannot connect /dev/nbd0"
    exit 0
fi
NBD_CONNECTED=1
cleanup_nbd() {
    if [[ "${NBD_CONNECTED:-0}" == 1 ]]; then
        qemu-nbd --disconnect /dev/nbd0 >/dev/null 2>&1 || true
    fi
    echo "  asserts: $E2E_PASS passed, $E2E_FAIL failed"
}
trap cleanup_nbd EXIT

rm -f /opt/e2e/guest-rootfs/etc/resolv.conf
cp /etc/resolv.conf /opt/e2e/guest-rootfs/etc/resolv.conf
printf 'size=+, type=83\n' | sfdisk /dev/nbd0 >/dev/null 2>&1
sleep 1
# No udev in the container: create the partition node the way udev would,
# deriving the minor from the whole-device node (minor + partition index).
MAJ=$(printf '%d' "0x$(stat -c %t /dev/nbd0)")
MIN=$(printf '%d' "0x$(stat -c %T /dev/nbd0)")
mknod /dev/nbd0p1 b "$MAJ" $((MIN + 1))
[[ -e /dev/nbd0p1 ]] || fail "partition /dev/nbd0p1 did not appear after sfdisk"
mkfs.ext4 -q -F -d /opt/e2e/guest-rootfs /dev/nbd0p1
qemu-nbd --disconnect /dev/nbd0 >/dev/null 2>&1 || true
NBD_CONNECTED=0
pass "crafted a partitioned ext4 guest disk"

echo "  [deep: offline package install/remove/list on a Linux instance]"
mkdir -p "$WORK/linux"
cp "$GUEST_QCOW" "$WORK/linux/linux-disk.qcow2"
touch "$WORK/linux/empty.iso"
qemu-img create -f raw "$WORK/linux/VARS.fd" 4M >/dev/null 2>&1
cat > "$WORK/linux/instance.toml" <<EOF
name = "guest-linux"
iso_path = "$WORK/linux/empty.iso"
disk_path = "$WORK/linux/linux-disk.qcow2"
ovmf_vars_path = "$WORK/linux/VARS.fd"

[gpu]
render_backend = "Cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
display_engine = "None"
fullscreen = false

[audio]
backend = "None"
EOF
LID="$(andler create --file "$WORK/linux/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$LID" ]] || fail "empty id from linux guest create"
pass "linux guest instance created"

expect_ok "guest list (offline)" -- andler guest list "$LID"
expect_out_grep "lists qemu-guest-agent" "qemu-guest-agent"
expect_out_grep "nothing installed yet" "not installed"

expect_ok "guest install qemu-guest-agent (offline apt)" -- timeout 300 andler guest install qemu-guest-agent "$LID"
expect_out_grep "install reports success" "installed successfully"

expect_ok "guest list after install" -- andler guest list "$LID"
expect_out_grep "qemu-ga now installed" "qemu-guest-agent.*installed"

expect_ok "guest remove qemu-guest-agent" -- timeout 300 andler guest remove qemu-guest-agent "$LID"
expect_out_grep "remove reports success" "removed successfully"

expect_ok "guest list after remove" -- andler guest list "$LID"
expect_out_grep "qemu-ga removed again" "qemu-guest-agent.*not installed"

echo "  [deep: Android boot-mode switching]"
AID="$(create_android "$WORK/ai" guest-android "$GUEST_QCOW")"
[[ -n "$AID" ]] || fail "empty id from android create"
pass "android guest instance created"

expect_ok "guest list android (offline)" -- andler guest list "$AID"
expect_out_grep "lists libndk" "libndk"
expect_out_grep "translators not installed" "not installed"

# No --translator-dir would trigger a network download of the translator
# (unbounded reqwest) before the waydroid check fails — pass a plain local
# path so the test stays deterministic offline. The waydroid-system check
# runs before the translator files are ever read, so the path is never
# created or opened.
expect_fail "guest install libndk fails without a waydroid system" -- andler guest install libndk "$AID" --translator-dir "/cache/translator"

expect_ok "boot-mode switch to android" -- andler guest boot-mode "$AID" android
expect_out_grep "switch reports success" "Boot mode switched"
expect_ok "boot-mode get" -- andler guest boot-mode "$AID"
expect_out_grep "boot mode is android" "^android$"

expect_ok "boot-mode switch back to linux" -- andler guest boot-mode "$AID" linux
expect_ok "boot-mode get again" -- andler guest boot-mode "$AID"
expect_out_grep "boot mode is linux" "^linux$"

echo "  [cleanup]"
expect_ok "remove android guest instance" -- andler remove "$AID" --purge
expect_ok "remove linux guest instance" -- andler remove "$LID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
