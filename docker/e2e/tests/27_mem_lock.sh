#!/usr/bin/env bash
# 27 — memory.mem_lock: the config key round-trips through
# `config set`/`config view`, is false by default, rejects malformed
# values, and survives a daemon restart. The QEMU arg it maps to
# (`-overcommit mem-lock=on`) is covered by the cmdline unit test; the
# e2e suite exercises the user-visible config surface.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")"'/common.sh'

WORK="$E2E_WORKDIR/27-mem-lock"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-mem-lock)"
[[ -n "$ID" ]] || fail "empty instance id from create"
pass "created instance $ID"

echo "  [default is false]"
expect_ok "config view shows default" -- andler config view "$ID"
expect_out_grep "mem_lock defaults to false" "mem_lock: false"
pass "mem_lock defaults to false"

echo "  [config set true]"
expect_ok "config set mem_lock true" -- andler config set "$ID" memory.mem_lock true
expect_ok "config view reflects true" -- andler config view "$ID"
expect_out_grep "mem_lock set to true" "mem_lock: true"
pass "mem_lock set to true and visible in config view"

echo "  [config set false]"
expect_ok "config set mem_lock false" -- andler config set "$ID" memory.mem_lock false
expect_ok "config view reflects false" -- andler config view "$ID"
expect_out_grep "mem_lock set back to false" "mem_lock: false"
pass "mem_lock set back to false"

echo "  [malformed value rejected]"
expect_fail "config set rejects non-bool" -- andler config set "$ID" memory.mem_lock banana
expect_err_grep "malformed value error" "expected true or false"

echo "  [survives daemon restart]"
expect_ok "config set true before restart" -- andler config set "$ID" memory.mem_lock true
expect_ok "stop daemon" -- bash -c "kill \$(cat \"$E2E_DAEMON_PID_FILE\") 2>/dev/null; sleep 0.5"
start_daemon
expect_ok "config view after restart shows true" -- andler config view "$ID"
expect_out_grep "mem_lock persisted across restart" "mem_lock: true"
pass "mem_lock persisted across daemon restart"

expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances remain" "no instances"
