#!/usr/bin/env bash
# 12 — process reconnect: a qemu process that survives a daemon SIGKILL is
# adopted by the restarted daemon (Running, not Error), can be stopped and
# removed, and leaves no stray qemu behind.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/12-reconnect"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-reconnect)"
[[ -n "$ID" ]] || fail "empty instance id from create --file"

expect_ok "start" -- andler start "$ID"
expect_ok "status shows Running initially" -- andler status "$ID"
expect_out_grep "status Running" "Running"

QEMU_PID="$(pgrep -f "qemu-system.*e2e-reconnect" | head -1 || true)"
[[ -n "$QEMU_PID" ]] || fail "could not find the qemu process for the started instance"
pass "found qemu pid $QEMU_PID"

DAEMON_PID="$(cat "$E2E_DAEMON_PID_FILE")"
[[ -n "$DAEMON_PID" ]] || fail "no daemon pid recorded"
echo "  [SIGKILL the daemon under a running instance]"
kill -9 "$DAEMON_PID" || true
# The killed daemon lingers as a zombie until its parent (the orchestrator)
# reaps it, so "gone" means: reaped (no /proc entry) or in zombie state —
# kill -0 stays true for zombies.
for _ in $(seq 1 20); do
    stat="$(ps -o stat= -p "$DAEMON_PID" 2>/dev/null || true)"
    [[ -z "$stat" || "$stat" == Z* ]] && break
    sleep 0.1
done
GONE=1
stat="$(ps -o stat= -p "$DAEMON_PID" 2>/dev/null || true)"
if [[ -z "$stat" || "$stat" == Z* ]]; then GONE=0; fi
if [[ "$GONE" -ne 0 ]]; then
    fail "daemon did not die from SIGKILL"
fi
pass "daemon killed"

sleep 1
if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    fail "qemu died together with the daemon (expected it to survive SIGKILL)"
fi
pass "qemu survived the daemon SIGKILL"

echo "  [restart the daemon on the same store]"
ANDLERD_STORE_PATH="$E2E_STORE_PATH" ANDLERD_LISTEN_ADDR="$E2E_LISTEN_ADDR" \
    RUST_LOG=info "$ANDLERD_BIN" >>"$E2E_WORKDIR/daemon.log" 2>&1 &
echo "$!" >"$E2E_DAEMON_PID_FILE"
for _ in $(seq 1 50); do
    if andler list >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done
andler list >/dev/null 2>&1 || fail "restarted daemon did not become ready"

RECONNECTED=1
for _ in $(seq 1 24); do
    if andler status "$ID" >"$E2E_LAST_OUT" 2>/dev/null &&
        grep -q "Running" "$E2E_LAST_OUT"; then
        RECONNECTED=0
        break
    fi
    sleep 0.5
done
[[ "$RECONNECTED" -eq 0 ]] || fail "instance did not come back as Running after daemon restart"
pass "instance reconnected to the surviving qemu (Running)"

expect_ok "stop the adopted instance" -- andler stop "$ID"
expect_ok "status shows Stopped after adopt" -- andler status "$ID"
expect_out_grep "Stopped" "Stopped"

for _ in $(seq 1 20); do
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.5
done
if kill -0 "$QEMU_PID" 2>/dev/null; then
    fail "adopted qemu still alive after stop"
fi
pass "adopted qemu terminated by stop"

expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"