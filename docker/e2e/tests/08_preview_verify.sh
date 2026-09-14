#!/usr/bin/env bash
# 08 — client-side commands: create --dry-run (preview), --verify,
# the wizard's non-TTY behavior, shell completions, and `andler doctor`.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/08-preview"
mkdir -p "$WORK"
qemu-img create -f qcow2 "$WORK/disk.qcow2" 2G >/dev/null 2>&1
touch "$WORK/empty.iso"
qemu-img create -f raw "$WORK/VARS.fd" 4M >/dev/null 2>&1

echo "  [create --dry-run]"
expect_ok "dry-run resolves a Linux VM" -- andler create --kind linux --name dry --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --dry-run
expect_out_grep "dry-run header" "Dry run: this would create"
expect_out_grep "dry-run kind" "Type: *Linux VM"
expect_out_grep "dry-run disk line" "^Disk:"
    expect_ok "create --json dry-run" -- andler create --kind linux --name dry-json --iso-path "$WORK/empty.iso" --disk-path "$WORK/dry-json.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --dry-run --json
    cp "$E2E_LAST_OUT" "$WORK/dry-run.json"
    expect_ok "create --json dry-run reports name" -- jq -e '.name == "dry-json"' "$WORK/dry-run.json"
    expect_ok "create --json dry-run reports LinuxVm kind" -- jq -e '.kind | has("LinuxVm")' "$WORK/dry-run.json"

expect_ok "nothing was created by --dry-run" -- andler list
expect_out_grep "no instances" "no instances"

echo "  [create --template]"
expect_ok "headless template applies" -- andler create --kind linux --name tpl --iso-path "$WORK/empty.iso" --disk-path "$WORK/tpl.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --template headless --dry-run
expect_out_grep "template gpu is Cpu" "GPU:.*Cpu"
expect_out_grep "template display is headless" "Display:.*None"
expect_out_grep "template audio is off" "Audio:.*None"

expect_ok "desktop template applies" -- andler create --kind linux --name tpl2 --iso-path "$WORK/empty.iso" --disk-path "$WORK/tpl2.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --template desktop --dry-run
expect_out_grep "desktop template gpu is Venus" "GPU:.*Venus"
expect_out_grep "desktop template display is Sdl" "Display:.*Sdl"

expect_fail "unknown template is rejected" -- andler create --kind linux --name tpl3 --iso-path "$WORK/empty.iso" --disk-path "$WORK/tpl3.qcow2" --template not-a-template --dry-run
expect_err_grep "template error lists builtins" "built-ins: headless, desktop"

expect_fail "template is rejected for android" -- andler create --kind android --name tpl4 --android-version 13 --template headless --dry-run
expect_err_grep "android template error" "only supported with --kind linux"

expect_ok "nothing was created by templates" -- andler list
expect_out_grep "no instances" "no instances"

echo "  [create --verify]"
expect_ok "verify passes on a valid config" -- andler create --kind linux --name ver --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify
expect_ok "verify --json passes on valid config" -- andler create --kind linux --name verjson --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify --json
cp "$E2E_LAST_OUT" "$WORK/verify-ok.json"
expect_ok "verify --json reports passed=true" -- jq -e '.passed == true' "$WORK/verify-ok.json"
expect_ok "verify --json reports name" -- jq -e '.name == "verjson"' "$WORK/verify-ok.json"
expect_ok "verify --json reports checks array" -- jq -e '.checks | type == "array"' "$WORK/verify-ok.json"
expect_ok "verify --json reports check entries" -- jq -e '.checks | all(.ok == true)' "$WORK/verify-ok.json"

expect_fail "verify fails on a missing iso" -- andler create --kind linux --name ver2 --iso-path "$WORK/missing.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify
expect_fail "verify --json fails on a missing iso" -- andler create --kind linux --name ver3 --iso-path "$WORK/missing.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify --json
expect_ok "verify --json reports passed=false" -- jq -e '.passed == false' "$E2E_LAST_OUT"

expect_ok "nothing was created by --verify" -- andler list
expect_out_grep "no instances" "no instances"

echo "  [wizard in a non-TTY environment]"
export ANDLER_WIZARD_NOT_TTY=1
expect_fail "wizard refuses to run without a TTY" -- andler create --kind linux --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2"
expect_err_grep "wizard error mentions the tty" "no TTY"
unset ANDLER_WIZARD_NOT_TTY

echo "  [completions]"
expect_ok "bash completions generate" -- andler completions bash
if [[ -s "$E2E_LAST_OUT" ]]; then
    pass "completions output is non-empty"
else
    fail "completions output is non-empty"
fi

echo "  [doctor]"
# doctor exits non-zero on any warning, and a container has no base images
# by default — seed a fixture so the offline checks can be asserted too.
BIMG_DIR="$HOME/.andler/cache/base-images/android13-vanilla"
mkdir -p "$BIMG_DIR"
printf '{"android_major":"13","android_variant":"vanilla","built_at":"e2e-fixture"}' > "$BIMG_DIR/e2e-fake.manifest.json"
touch "$BIMG_DIR/e2e-fake.qcow2"
set +e
andler doctor >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR"
DOCTOR_RC=$?
set -e
if [[ "$DOCTOR_RC" -eq 0 || "$DOCTOR_RC" -eq 1 ]]; then
    pass "doctor runs (exit $DOCTOR_RC; 1 is expected when offline/base-image checks warn)"
else
    fail "doctor exited with $DOCTOR_RC"
fi
expect_out_grep "doctor hypervisor section" "^Hypervisor"
expect_out_grep "doctor daemon section" "^Daemon"
expect_out_grep "doctor checks /dev/kvm" "/dev/kvm"
expect_out_grep "doctor checks qemu" "qemu-system-x86_64"
expect_out_grep "doctor checks CAP_NET_ADMIN" "CAP_NET_ADMIN"

echo "  [doctor offline prereqs (zero-root)]"
expect_out_grep "offline section present" "Offline guest operations"
expect_out_grep "guestfish checked" "guestfish"
expect_out_nogrep "no FUSE prerequisite is claimed any more" "guestmount"
expect_out_nogrep "no /dev/fuse prerequisite is claimed any more" "/dev/fuse"
expect_out_grep "isolated network section present" "Isolated network mode"
expect_out_grep "userns checked where it is actually needed" "user namespaces"
expect_out_nogrep "no sudoers/helper checks remain" "sudoers"
expect_out_nogrep "no nbd kernel module check remains" "nbd kernel module"
expect_out_nogrep "no andler-helper check remains" "andler-helper"