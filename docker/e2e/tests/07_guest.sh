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
# Give the fixture a waydroid-style system image carrying a base build.prop,
# so the translator install exercises the real merge path (upper build.prop
# = base props + translator props) instead of the empty-base fallback.
SYSIMG="$WORK/system.img"
mke2fs -q -F -t ext4 "$SYSIMG" 8M >/dev/null 2>&1
mkdir -p "$WORK/sysbp" /opt/e2e/guest-rootfs/etc/waydroid-extra/images
printf 'ro.product.model=WaydroidE2E\nro.build.version.sdk=33\n' > "$WORK/sysbp/build.prop"
debugfs -w -R "mkdir /system" "$SYSIMG" >/dev/null 2>&1
debugfs -w -R "write $WORK/sysbp/build.prop /system/build.prop" "$SYSIMG" >/dev/null 2>&1
cp "$SYSIMG" /opt/e2e/guest-rootfs/etc/waydroid-extra/images/system.img
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
render_backend = "cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
engine = "none"
fullscreen = false

[audio]
backend = "none"
EOF
LID="$(andler create --file "$WORK/linux/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$LID" ]] || fail "empty id from linux guest create"
pass "linux guest instance created"

expect_ok "guest list (offline)" -- andler guest list "$LID"
expect_out_grep "lists qemu-guest-agent" "qemu-guest-agent"
expect_out_grep "nothing installed yet" "not installed"

expect_ok "guest install qemu-guest-agent (offline apt)" -- timeout 300 andler guest install qemu-guest-agent "$LID" --offline
expect_out_grep "install reports success" "installed successfully"

expect_ok "guest list after install" -- andler guest list "$LID"
expect_out_grep "qemu-ga now installed" "qemu-guest-agent.*installed"
    expect_ok "guest list --json" -- timeout 120 andler guest list "$LID" --json
    cp "$E2E_LAST_OUT" "$WORK/guest-list.json"
    expect_ok "guest list --json reports an array of packages" -- jq -e '.packages | type == "array"' "$WORK/guest-list.json"
    expect_ok "guest list --json reports qemu-guest-agent installed" -- jq -e '.packages[] | select(.name == "qemu-guest-agent") | .status == "installed"' "$WORK/guest-list.json"

expect_ok "guest remove qemu-guest-agent" -- timeout 300 andler guest remove qemu-guest-agent "$LID" --offline
expect_out_grep "remove reports success" "removed successfully"

expect_ok "guest list after remove" -- andler guest list "$LID"
expect_out_grep "qemu-ga removed again" "qemu-guest-agent.*not installed"

echo "  [smart path: a stopped VM whose agent never answers falls back to the appliance]"
# This VM does not boot (no guest agent ever answers), so the maintenance boot
# runs out its whole wait budget. The daemon used to stop there and tell the
# operator to re-run with --offline; it now does that itself, in the same
# command.
expect_ok "guest install without --offline falls back to the appliance" -- timeout 400 andler guest install qemu-guest-agent "$LID"
expect_out_grep "the fallback install reports success" "installed successfully"
expect_ok "the fallback left the instance stopped" -- andler status "$LID"
expect_out_grep "instance is stopped" "Stopped"
expect_ok "guest list shows the fallback install" -- timeout 120 andler guest list "$LID"
expect_out_grep "qemu-ga installed by the fallback" "qemu-guest-agent.*installed"

echo "  [deep: provision manifest applies offline via the guestfs appliance]"
mkdir -p "$WORK/provision"
printf 'payload-bytes\n' > "$WORK/provision/payload.bin"
cat > "$WORK/provision/manifest.toml" <<'MANIFEST'
schema_version = 1
name = "e2e-provision"

[[ops]]
op = "mkdir-p"
path = "/opt/andler-e2e"

[[ops]]
op = "write-file"
path = "/opt/andler-e2e/provisioned.txt"
content = "provisioned by e2e\n"
mode = "0640"

[[ops]]
op = "upload-file"
guest_path = "/opt/andler-e2e/payload.bin"
host_path = "./payload.bin"
mode = "0755"

[[ops]]
op = "symlink"
target = "/opt/andler-e2e/provisioned.txt"
link = "/opt/andler-e2e/link.txt"
MANIFEST

expect_ok "guest provision applies the manifest offline" -- timeout 300 andler guest provision "$WORK/provision/manifest.toml" "$LID"
expect_out_grep "provision reports success" 'Provisioned .e2e-provision. \(6 ops\) successfully'

if ! qemu-nbd --connect=/dev/nbd0 "$WORK/linux/linux-disk.qcow2"; then
    fail "cannot re-connect the linux disk for provision verification"
fi
blockdev --rereadpt /dev/nbd0
sleep 1
NBD_CONNECTED=1
mkdir -p "$WORK/mnt"
mount -o ro /dev/nbd0p1 "$WORK/mnt"
P_DIR="$WORK/mnt/opt/andler-e2e"
[[ -f "$P_DIR/provisioned.txt" ]] && pass "write-file landed" || fail "write-file missing"
grep -q "^provisioned by e2e$" "$P_DIR/provisioned.txt" \
    && pass "write-file content" || fail "write-file content mismatch"
MODE=$(stat -c %a "$P_DIR/provisioned.txt")
[[ "$MODE" == "640" ]] && pass "write-file mode 0640" || fail "write-file mode is $MODE (want 640)"
[[ -f "$P_DIR/payload.bin" ]] && pass "upload-file landed" || fail "upload-file missing"
grep -q "^payload-bytes$" "$P_DIR/payload.bin" \
    && pass "upload-file content" || fail "upload-file content mismatch"
MODE=$(stat -c %a "$P_DIR/payload.bin")
[[ "$MODE" == "755" ]] && pass "upload-file mode 0755" || fail "upload-file mode is $MODE (want 755)"
[[ -L "$P_DIR/link.txt" ]] && pass "symlink created" || fail "symlink missing"
[[ "$(readlink "$P_DIR/link.txt")" == "/opt/andler-e2e/provisioned.txt" ]] \
    && pass "symlink target" || fail "symlink target mismatch"
umount "$WORK/mnt"
qemu-nbd --disconnect /dev/nbd0 >/dev/null 2>&1 || true
NBD_CONNECTED=0
pass "provision verified through the mounted disk"

echo "  [deep: Android boot-mode switching]"
AID="$(create_android "$WORK/ai" guest-android "$GUEST_QCOW")"
[[ -n "$AID" ]] || fail "empty id from android create"
pass "android guest instance created"

expect_ok "guest list android (offline)" -- andler guest list "$AID"
expect_out_grep "lists libndk" "libndk"
expect_out_grep "translators not installed" "not installed"

# A waydroid overlay upper dir does not exist before the guest's first boot,
# and a real install would download ~18 MiB of prebuilts. Craft a local
# translator fixture with the real NDK archive layout (bin/, etc/binfmt_misc,
# etc/init, lib/, lib64/, plus the top-level libndk* libraries) and install
# from it — this exercises the full pre-boot path: overlay dir creation,
# wildcard expansion, stage-then-move, props write, and the rc write.
echo "  [deep: ARM translator install from a local fixture]"
XLATE="$WORK/xfi-ndk"
mkdir -p "$XLATE"/bin/arm "$XLATE"/bin/arm64 \
    "$XLATE"/etc/binfmt_misc "$XLATE"/etc/init \
    "$XLATE"/lib/arm "$XLATE"/lib64/arm64
printf '\x7fELF' > "$XLATE/bin/arm/app_process"
printf '\x7fELF' > "$XLATE/bin/arm64/app_process64"
printf '\x7fELF' > "$XLATE/bin/ndk_translation_program_runner_binfmt_misc"
printf '\x7fELF' > "$XLATE/bin/ndk_translation_program_runner_binfmt_misc_arm64"
printf ':arm_exe:M::\x7fELF\x02\x01\x01\x00' > "$XLATE/etc/binfmt_misc/arm_exe"
printf ':arm_dyn:M::\x7fELF\x02\x01\x01\x00' > "$XLATE/etc/binfmt_misc/arm_dyn"
printf ':arm64_exe:M::\x7fELF\x02\x01\x01\x00' > "$XLATE/etc/binfmt_misc/arm64_exe"
printf ':arm64_dyn:M::\x7fELF\x02\x01\x01\x00' > "$XLATE/etc/binfmt_misc/arm64_dyn"
# The real ndk_translation.rc is byte-for-byte the file shipped in the
# prebuilt archives (both the android-11 and android-13 pins carry this exact
# content) — the fixture must be a functional rc so the binfmt registration
# actually happens on guest boot, not a stub that does nothing.
cat > "$XLATE/etc/init/ndk_translation.rc" <<'EOF'
# Enable native bridge for target executables
on early-init && property:ro.enable.native.bridge.exec=1
    mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc

on property:ro.enable.native.bridge.exec=1 && property:ro.dalvik.vm.isa.arm=x86
    copy /system/etc/binfmt_misc/arm_exe /proc/sys/fs/binfmt_misc/register
    copy /system/etc/binfmt_misc/arm_dyn /proc/sys/fs/binfmt_misc/register

on property:ro.enable.native.bridge.exec=1 && property:ro.dalvik.vm.isa.arm64=x86_64
    copy /system/etc/binfmt_misc/arm64_exe /proc/sys/fs/binfmt_misc/register
    copy /system/etc/binfmt_misc/arm64_dyn /proc/sys/fs/binfmt_misc/register
EOF
printf 'Architecture: ARMv7\n' > "$XLATE/etc/cpuinfo.arm.txt"
printf 'Architecture: AArch64\n' > "$XLATE/etc/cpuinfo.arm64.txt"
echo 'arm { }' > "$XLATE/etc/ld.config.arm.txt"
echo 'arm64 { }' > "$XLATE/etc/ld.config.arm64.txt"
printf '\x7fELF' > "$XLATE/lib/libndk_translation.so"
printf '\x7fELF' > "$XLATE/lib/libndk_proxy.so"
printf '\x7fELF' > "$XLATE/lib/libndk_extra.so"
printf '\x7fELF' > "$XLATE/lib64/libndk_translation.so"
printf '\x7fELF' > "$XLATE/lib64/libndk_proxy.so"
printf '\x7fELF' > "$XLATE/lib/arm/app_process"
printf '\x7fELF' > "$XLATE/lib64/arm64/app_process64"

expect_ok "guest install libndk pre-boot from fixture" -- andler guest install libndk "$AID" --translator-dir "$XLATE"
expect_out_grep "install reports success" "Translator .libndk. installed"
expect_ok "guest list shows libndk installed" -- andler guest list "$AID"
expect_out_grep "libndk is now installed" "libndk.*installed"
expect_ok "guest install libndk again is idempotent" -- andler guest install libndk "$AID" --translator-dir "$XLATE"
expect_out_grep "reinstall reports success" "Translator .libndk. installed"

echo "  [deep: upper build.prop merges base + translator props]"
# Regression: an upper build.prop containing only translator props shadows
# the base image's build.prop wholesale and breaks Android boot (waydroid
# can't parse the android version, zygote/ART crash). The upper file must
# contain the base image's props (from system.img) merged with the
# translator's managed props.
# `create` prints the short id, but the instance directory uses the full
# 64-hex id — resolve the actual disk path instead of composing it.
INST_DISK="$(ls "$WORK/ai/android-instances/"*/disk.qcow2 2>/dev/null | head -n1)"
[[ -n "$INST_DISK" ]] || fail "android instance disk not found under $WORK/ai/android-instances/"
if ! qemu-nbd --connect=/dev/nbd0 "$INST_DISK"; then
    fail "cannot re-connect the android instance disk"
fi
# The kernel only rescans a re-used /dev/nbd* when the partition table is
# rewritten — the first connect got its p1 node from the sfdisk BLKRRPART.
# Without the reread the second connect's partition never appears (there is
# no udev in the container to trigger it either).
blockdev --rereadpt /dev/nbd0
sleep 1
NBD_CONNECTED=1
mkdir -p "$WORK/mnt"
mount -o ro /dev/nbd0p1 "$WORK/mnt"
BP="$WORK/mnt/var/lib/waydroid/overlay/system/build.prop"
grep -q "^ro.product.model=WaydroidE2E$" "$BP" \
    && pass "upper build.prop keeps the base image's props" \
    || fail "upper build.prop must keep base props (found: $(head -c 300 "$BP" | tr '\n' ' '))"
grep -q "^ro.build.version.sdk=33$" "$BP" \
    && pass "upper build.prop keeps ro.build.version.sdk" \
    || fail "upper build.prop must keep ro.build.version.sdk"
grep -q "^ro.dalvik.vm.native.bridge=libndk_translation.so$" "$BP" \
    && pass "upper build.prop has translator props" \
    || fail "upper build.prop must carry the translator props"
umount "$WORK/mnt"
qemu-nbd --disconnect /dev/nbd0 >/dev/null 2>&1 || true
NBD_CONNECTED=0

expect_ok "boot-mode switch to android" -- andler guest boot-mode "$AID" android
expect_out_grep "switch reports success" "Boot mode switched"
expect_ok "boot-mode get" -- andler guest boot-mode "$AID"
expect_out_grep "boot mode is android" "^android$"

expect_ok "boot-mode switch back to linux" -- andler guest boot-mode "$AID" linux
expect_ok "boot-mode get again" -- andler guest boot-mode "$AID"
expect_out_grep "boot mode is linux" "^linux$"

echo "  [deep: guest apply installs what the instance config selects]"
# `guest apply` is what `andler create` (wizard) runs right after creating a
# VM: it derives the guest-side work from the instance's own config and
# reports one classified outcome per selection. The translator branch is
# driven from the local fixture cache (seeded below) so the run needs no
# network, exactly like the install above.
NDK_CACHE="$HOME/.andler/cache/arm-translators/ndk"
mkdir -p "$NDK_CACHE"
cp -r "$XLATE/." "$NDK_CACHE/"
pass "seeded the translator cache from the local fixture"
# Record libndk as the instance's selection: `config set` switches the
# translator to what the config already carries (a no-op here — the fixture
# install above put it in place) and writes the value into instance.toml.
expect_ok "record libndk as the instance's ARM translator" -- \
    andler config set "$AID" kind.android_profile.arm_translator libndk
expect_ok "the config now selects libndk" -- andler config view "$AID"
expect_out_grep "translator recorded" "libndk"

expect_ok "guest apply on the Android instance" -- timeout 300 andler guest apply "$AID"
expect_out_grep "the translator selection is reported as already present" \
    "arm-translator: already present"

expect_ok "guest apply --json" -- andler guest apply "$AID" --json
cp "$E2E_LAST_OUT" "$WORK/apply.json"
expect_ok "json reports one entry per selection" -- \
    jq -e '.selections | map(.name) | index("arm-translator") != null' "$WORK/apply.json"
expect_ok "json reports the already-present status" -- \
    jq -e '.selections[] | select(.name == "arm-translator") | .status == "already_present"' "$WORK/apply.json"

echo "  [deep: guest apply installs the clipboard agent offline]"
# The Linux fixture disk is a real Debian rootfs. Disabling the Android
# instance's clipboard leaves its (waydroid) rootfs untouched, so the package
# branch is exercised once, deterministically, on the Linux instance.
expect_ok "disable clipboard on the Android instance" -- \
    andler config set "$AID" input.clipboard_enabled false
expect_ok "guest apply on the Linux instance" -- timeout 600 andler guest apply "$LID"
expect_out_grep "clipboard agent selection applied" "spice-vdagent: applied"
expect_ok "guest list confirms the agent" -- andler guest list "$LID"
expect_out_grep "spice-vdagent is installed" "spice-vdagent.*installed"

echo "  [cleanup]"
expect_ok "remove android guest instance" -- andler remove "$AID" --purge
expect_ok "remove linux guest instance" -- andler remove "$LID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
