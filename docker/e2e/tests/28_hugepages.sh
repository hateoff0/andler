#!/usr/bin/env bash
# 28 — memory.hugepages: the config key round-trips through
# `config set`/`config view`, is false by default, rejects malformed
# values, and survives a daemon restart. The QEMU arg it maps to
# (`memory-backend-file` with `mem-path=/dev/hugepages`) is covered by the
# cmdline unit test; the e2e suite exercises the user-visible config surface.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/28-hugepages"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-hugepages)"
[[ -n "$ID" ]] || fail "empty instance id from create"
pass "created instance $ID"

echo "  [default is false]"
expect_ok "config view shows default" -- andler config view "$ID"
expect_out_grep "hugepages defaults to false" "hugepages: false"
pass "hugepages defaults to false"

echo "  [config set true]"
expect_ok "config set hugepages true" -- andler config set "$ID" memory.hugepages true
expect_ok "config view reflects true" -- andler config view "$ID"
expect_out_grep "hugepages set to true" "hugepages: true"
pass "hugepages set to true and visible in config view"

echo "  [config set false]"
expect_ok "config set hugepages false" -- andler config set "$ID" memory.hugepages false
expect_ok "config view reflects false" -- andler config view "$ID"
expect_out_grep "hugepages set back to false" "hugepages: false"
pass "hugepages set back to false"

echo "  [malformed value rejected]"
expect_fail "config set rejects non-bool" -- andler config set "$ID" memory.hugepages banana
expect_err_grep "malformed value error" "expected true or false"

echo "  [survives daemon restart]"
expect_ok "config set true before restart" -- andler config set "$ID" memory.hugepages true
expect_ok "stop daemon" -- bash -c "kill \$(cat \"$E2E_DAEMON_PID_FILE\") 2>/dev/null; sleep 0.5"
start_daemon
expect_ok "config view after restart shows true" -- andler config view "$ID"
expect_out_grep "hugepages persisted across restart" "hugepages: true"
pass "hugepages persisted across daemon restart"

expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances remain" "no instances"
