#!/usr/bin/env bash
# 15 — andler connect: serial console attach over the unix chardev socket,
# state machine negatives, console.log tee.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/15-connect"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-connect)"
[[ -n "$ID" ]] || fail "empty instance id from create"

echo "  [state machine negatives]"
expect_fail "connect to a stopped instance" -- andler connect "$ID" --level console
expect_err_grep "connect-while-stopped message" "start it first"

echo "  [console attach on a running instance]"
expect_ok "start" -- andler start "$ID"
# The OVMF firmware only echoes serial output while waiting for a key
# ("Press any key to enter the Boot Manager Menu"); attach after it
# reaches that phase, then retry until the chardev socket is bound.
sleep 2
# Firmware output is only produced once: a client that attaches after the
# firmware has finished sees the UEFI shell prompt instead of the banner, so
# the window is long enough to contain one of them either way.
expect_ok "console attach relays guest output" -- bash -c '
    rc=1
    for i in $(seq 1 6); do
        (sleep 1; printf "\r") | timeout 8 andler connect "$ID" --level console > "$E2E_LAST_OUT" 2>/dev/null
        rc=${PIPESTATUS[1]}
        if [[ $rc -eq 124 || $rc -eq 0 ]]; then
            break
        fi
        sleep 0.5
    done
    [[ $rc -eq 124 || $rc -eq 0 ]]'
expect_out_grep "guest serial output arrives" "(BdsDxe|Shell>|Press ESC)"

expect_fail "unknown instance is rejected" -- andler connect "$UNKNOWN_ID" --level console
expect_err_grep "unknown-instance message" "no instance found"

echo "  [console.log tee]"
if [[ -s "$WORK/console.log" ]]; then
    pass "console.log captures the serial stream"
else
    fail "console.log missing or empty (serial chardev logfile)"
fi

echo "  [cleanup]"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
