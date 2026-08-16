#!/usr/bin/env bash
# docker/e2e/e2e.sh — E2E suite orchestrator.
#
# Starts a fresh andlerd (isolated store) and runs every tests/NN_*.sh suite
# in order, collecting a per-suite pass/fail summary. Each suite is
# self-contained: it creates and removes its own instances. Exit code is
# non-zero when any suite fails.
#
# Run inside the container (`docker compose -f docker/e2e/compose.yaml run
# --rm e2e`) or directly from a checkout with ANDLER_BIN/ANDLERD_BIN set.
#
# Environment:
#   E2E_LISTEN_ADDR   daemon listen address (default 127.0.0.1:50051)
#   E2E_DEEP_GUEST    0 disables the deep guest tests (qemu-nbd based)
#   ANDLER_BIN / ANDLERD_BIN   binary paths (default /usr/local/bin/*)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -d "$SCRIPT_DIR/tests" ]]; then
    E2E_ROOT="$SCRIPT_DIR"
else
    E2E_ROOT="/usr/local/share/andler-e2e"
fi

export E2E_WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/andler-e2e.XXXXXX")"
export E2E_STORE_PATH="$E2E_WORKDIR/andlerd.db"
export E2E_LISTEN_ADDR="${E2E_LISTEN_ADDR:-127.0.0.1:50051}"
export E2E_DAEMON_PID_FILE="$E2E_WORKDIR/daemon.pid"
export E2E_DEEP_GUEST="${E2E_DEEP_GUEST:-1}"
# Short maintenance auto-start wait: suite 07 exercises the "VM does not
# respond" path and must not spend 120s per attempt.
export ANDLERD_GUEST_AGENT_WAIT_SECS="${ANDLERD_GUEST_AGENT_WAIT_SECS:-30}"
export E2E_LAST_OUT="$E2E_WORKDIR/last.out"
export E2E_LAST_ERR="$E2E_WORKDIR/last.err"
export ANDLER_BIN="${ANDLER_BIN:-/usr/local/bin/andler}"
export ANDLERD_BIN="${ANDLERD_BIN:-/usr/local/bin/andlerd}"

source "$E2E_ROOT/tests/common.sh"

cleanup() {
    stop_daemon
    rm -rf "$E2E_WORKDIR"
}
trap cleanup EXIT

echo "==> andler e2e suite"
echo "    workdir: $E2E_WORKDIR"
echo "    daemon:  http://$E2E_LISTEN_ADDR (store: $E2E_STORE_PATH)"
echo "    deep guest tests: $([[ "$E2E_DEEP_GUEST" == 1 ]] && echo enabled || echo disabled)"

start_daemon

TOTAL=0
FAILED=0
SUITE=0
# Per-suite wall-clock budget. A suite that hangs (daemon wedged, CLI stuck
# on a dead connection) is killed here instead of stalling the whole run —
# with a diagnostics dump so the wedged state is inspectable in the log.
SUITE_TIMEOUT="${E2E_SUITE_TIMEOUT:-600}"
for script in "$E2E_ROOT"/tests/[0-9][0-9]_*.sh; do
    name="$(basename "$script")"
    echo
    echo "=== $name ==="
    # Fresh daemon + store per suite: a failing suite leaves instances
    # behind, but they can never leak into the next suite's assertions.
    SUITE=$((SUITE + 1))
    export E2E_STORE_PATH="$E2E_WORKDIR/store.$SUITE.db"
    stop_daemon
    start_daemon
    START_TS=$(date +%s%N)
    if timeout -k 10 "$SUITE_TIMEOUT" bash "$script"; then
        ELAPSED=$(awk -v a="$START_TS" -v b="$(date +%s%N)" 'BEGIN { printf "%.1f", (b-a)/1e9 }')
        echo "=== $name: PASS (${ELAPSED}s) ==="
        TOTAL=$((TOTAL + 1))
    else
        RC=$?
        ELAPSED=$(awk -v a="$START_TS" -v b="$(date +%s%N)" 'BEGIN { printf "%.1f", (b-a)/1e9 }')
        if [[ "$RC" -eq 124 ]]; then
            echo "=== $name: FAIL (timed out after ${ELAPSED}s, killed by watchdog) ==="
            echo "    --- remaining processes (possible wedge) ---"
            pgrep -af "andlerd|andler |qemu-system" | sed 's/^/      /' || true
            echo "    --- daemon.log tail ---"
            tail -20 "$E2E_WORKDIR/daemon.log" 2>/dev/null | sed 's/^/      /' || true
        else
            echo "=== $name: FAIL (${ELAPSED}s) ==="
        fi
        TOTAL=$((TOTAL + 1))
        FAILED=$((FAILED + 1))
    fi
    # A wedged suite may have left the daemon unkillable by SIGTERM; make
    # sure the next suite starts on a clean slate.
    if [[ -f "$E2E_DAEMON_PID_FILE" ]]; then
        local_pid="$(cat "$E2E_DAEMON_PID_FILE")"
        if [[ -n "$local_pid" ]] && kill -0 "$local_pid" 2>/dev/null; then
            kill -9 "$local_pid" 2>/dev/null || true
        fi
        rm -f "$E2E_DAEMON_PID_FILE"
    fi
done

stop_daemon

echo
echo "=== E2E SUMMARY: $((TOTAL - FAILED))/$TOTAL suites passed, $FAILED failed ==="
if [[ "$FAILED" -ne 0 ]]; then
    exit 1
fi
echo "ALL E2E SUITES PASSED"
