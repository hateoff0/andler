#!/usr/bin/env bash
# 11 — process supervision: a qemu process killed out from under the daemon
# lands the instance in Error { QEMU process exited unexpectedly } fast —
# via pidfd death notification, not the 30s health-check poll.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/11-supervision"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-supervision)"
[[ -n "$ID" ]] || fail "empty instance id from create --file"

expect_ok "start" -- andler start "$ID"
expect_ok "status shows Running initially" -- andler status "$ID"
expect_out_grep "status Running" "Running"

echo "  [kill qemu out from under the daemon]"
QEMU_PID="$(pgrep -f "qemu-system.*e2e-supervision" | head -1 || true)"
[[ -n "$QEMU_PID" ]] || fail "could not find the qemu process for the started instance"
pass "found qemu pid $QEMU_PID"
kill -9 "$QEMU_PID"

LOST_STATE=1
for _ in $(seq 1 24); do
    if andler status "$ID" --json >"$WORK/status.json" 2>/dev/null &&
        grep -q 'QEMU process exited unexpectedly' "$WORK/status.json"; then
        LOST_STATE=0
        break
    fi
    sleep 0.5
done
[[ "$LOST_STATE" -eq 0 ]] || fail "state did not become Error within 12s of SIGKILL"
pass "instance transitioned to Error promptly after qemu died"

expect_ok "status reports the exit reason" -- andler status "$ID"
expect_out_grep "exit message mentions the process" "QEMU process exited unexpectedly"

# qemu is silent on a working KVM boot (empty qemu.log), so the real
# contract is termination: the log stream must end once the process is
# dead, not hang waiting for lines that will never come. (No live-process
# `andler logs` here — that stream is open-ended and would hang the suite.)
expect_ok "log stream terminates after supervised death" -- timeout 10 "$ANDLER_BIN" --daemon-addr "http://${E2E_LISTEN_ADDR:?}" logs "$ID"

expect_ok "restart from Error is the documented recovery" -- andler start "$ID"
expect_ok "restarted status shows Running" -- andler status "$ID"
expect_out_grep "Running" "Running"

expect_ok "clean shutdown of the recovered instance" -- andler stop "$ID"
expect_ok "status shows Stopped" -- andler status "$ID"
expect_out_grep "Stopped" "Stopped"

expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"