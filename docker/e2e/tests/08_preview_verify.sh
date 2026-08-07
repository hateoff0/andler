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

expect_ok "nothing was created by --dry-run" -- andler list
expect_out_grep "no instances" "no instances"

echo "  [create --verify]"
expect_ok "verify passes on a valid config" -- andler create --kind linux --name ver --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify
expect_out_grep "verify reports all checks passed" "All checks passed"

expect_fail "verify fails on a missing iso" -- andler create --kind linux --name ver2 --iso-path "$WORK/missing.iso" --disk-path "$WORK/disk.qcow2" --ovmf-vars-template "$WORK/VARS.fd" --verify
expect_err_grep "verify error names the missing iso" "ISO file not found"

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
set +e
andler doctor >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR"
DOCTOR_RC=$?
set -e
if [[ "$DOCTOR_RC" -eq 0 || "$DOCTOR_RC" -eq 1 ]]; then
    pass "doctor runs (exit $DOCTOR_RC; 1 is expected when nbd/base-image checks warn)"
else
    fail "doctor exited with $DOCTOR_RC"
fi
expect_out_grep "doctor hypervisor section" "^Hypervisor"
expect_out_grep "doctor daemon section" "^Daemon"
expect_out_grep "doctor checks /dev/kvm" "/dev/kvm"
expect_out_grep "doctor checks qemu" "qemu-system-x86_64"

echo "  [doctor --fix installs andler-helper + single rule]"
# The container runs as root, so the helper check passes trivially; what
# --fix must still do is place the helper at the canonical path and write a
# sudoers file containing exactly one rule (no legacy per-binary rules).
if sudo -n true 2>/dev/null; then
    echo y | andler doctor --fix >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR" || {
        cat "$E2E_LAST_ERR" >&2
        fail "doctor --fix must exit 0"
    }
    if [[ -x /usr/local/sbin/andler-helper ]]; then
        pass "andler-helper installed at /usr/local/sbin/andler-helper"
    else
        fail "andler-helper installed at /usr/local/sbin/andler-helper"
    fi
    if [[ -f /etc/sudoers.d/andler ]]; then
        HELPER_RULES=$(grep -c "NOPASSWD: /usr/local/sbin/andler-helper" /etc/sudoers.d/andler || true)
        LEGACY_RULES=$(grep -cE "NOPASSWD: /(usr/bin|usr/sbin|bin|sbin)/(modprobe|qemu-nbd|mount|umount|chroot|mkdir|cp|mv|rm|chmod)( |,|$)" /etc/sudoers.d/andler || true)
        if [[ "$HELPER_RULES" -eq 1 ]]; then
            pass "sudoers file has exactly one andler-helper rule"
        else
            fail "sudoers file must contain exactly one andler-helper rule, found $HELPER_RULES"
        fi
        if [[ "$LEGACY_RULES" -eq 0 ]]; then
            pass "no legacy per-binary rules remain"
        else
            fail "legacy per-binary rules must be migrated away, found $LEGACY_RULES"
        fi
    else
        fail "/etc/sudoers.d/andler must exist after --fix"
    fi
else
    echo "    ok: sudo not usable in this container; skipping --fix assertions" >>"$E2E_LAST_OUT"
    pass "sudo not usable in this container; skipping --fix assertions"
fi