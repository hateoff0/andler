#!/usr/bin/env bash
# 02 — config: view, set (whitelisted keys), edit via $VISUAL, negatives.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/02-config"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-config)"
[[ -n "$ID" ]] || fail "empty instance id from create"

echo "  [config set]"
expect_ok "config set name" -- andler config set "$ID" name e2e-renamed
expect_ok "config view reflects new name" -- andler config view "$ID"
expect_out_grep "name was updated" "e2e-renamed"

expect_ok "config set display.resolution" -- andler config set "$ID" display.resolution 1600x900
expect_ok "config view reflects new resolution" -- andler config view "$ID"
expect_out_grep "resolution was updated" "resolution: 1600x900"

expect_fail "config set malformed resolution" -- andler config set "$ID" display.resolution banana
expect_err_grep "resolution error explains the format" "expects WxH"

expect_fail "config set unknown key" -- andler config set "$ID" bogus.key 1
expect_err_grep "unknown key error" "invalid config key"

expect_fail "config set of a nonexistent instance" -- andler config set "$UNKNOWN_ID" name x
expect_err_grep "config set error mentions not found" "not found"

echo "  [config edit via \$VISUAL]"
INSTANCE_TOML="$(echo "$HOME/.andler/instances/$ID"*/instance.toml)"
[[ -n "$INSTANCE_TOML" && -f "$INSTANCE_TOML" ]] || fail "instance.toml not found under $HOME/.andler/instances/$ID"
pass "instance.toml exists on disk"

export VISUAL="sed -i s/e2e-renamed/e2e-edited/ $INSTANCE_TOML"
expect_ok "config edit with a non-interactive editor" -- andler config edit "$ID"
expect_out_grep "edit reports success" "Config updated"
unset VISUAL

expect_ok "config view after edit" -- andler config view "$ID"
expect_out_grep "edited name persisted" "e2e-edited"

export VISUAL="sed -i s/name =/name =x/ $INSTANCE_TOML"
expect_fail "config edit with a broken TOML" -- andler config edit "$ID"
unset VISUAL

expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
