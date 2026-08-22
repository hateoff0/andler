#!/usr/bin/env bash
# 18 — boot_mode lives in instance.toml. The config exposes it without
# any offline disk mount, config set refuses to touch it (the dedicated
# boot-mode command is the only writer), and switch updates the config.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/18-boot-mode"
mkdir -p "$WORK"

echo "  [boot_mode surfaces from the config, not a disk mount]"
qemu-img create -f qcow2 "$WORK/base.qcow2" 2G >/dev/null 2>&1
AID="$(create_android "$WORK/ai" e2e-boot-mode "$WORK/base.qcow2")"
[[ -n "$AID" ]] || fail "empty id from android create"

expect_ok "config view" -- andler config view "$AID"
expect_out_grep "boot_mode present" "boot_mode: android"

expect_ok "config view is usable while stopped" -- andler config view "$AID"
expect_out_grep "boot_mode value" "android"

echo "  [boot_mode is immutable via config set]"
expect_fail "config set boot_mode is refused" -- andler config set "$AID" kind.android_profile.boot_mode linux
expect_err_grep "immutable reason" "dedicated boot-mode command"

expect_ok "get via guest boot-mode (config-backed)" -- andler guest boot-mode "$AID"
expect_out_grep "reports android" "android"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$AID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
