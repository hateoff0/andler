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
export E2E_LAST_OUT="$E2E_WORKDIR/last.out"
export E2E_LAST_ERR="$E2E_WORKDIR/last.err"
export ANDLER_BIN="${ANDLER_BIN:-/usr/local/bin/andler}"
export ANDLERD_BIN="${ANDLERD_BIN:-/usr/local/bin/andlerd}"

source "$E2E_ROOT/tests/common.sh"

# common.sh's andler() uses ANDLER_BIN; start_daemon hardcodes andlerd — make
# it honor the override by redefining locally after sourcing.
start_daemon() {
    echo "  starting andlerd (store: $E2E_STORE_PATH)"
    ANDLERD_STORE_PATH="$E2E_STORE_PATH" ANDLERD_LISTEN_ADDR="$E2E_LISTEN_ADDR" \
        RUST_LOG=info "$ANDLERD_BIN" >>"$E2E_WORKDIR/daemon.log" 2>&1 &
    echo "$!" >"$E2E_DAEMON_PID_FILE"
    for _ in $(seq 1 50); do
        if andler list >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.2
    done
    echo "    FAIL: andlerd did not become ready within 10s"
    exit 1
}

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
    if bash "$script"; then
        echo "=== $name: PASS ==="
        TOTAL=$((TOTAL + 1))
    else
        echo "=== $name: FAIL ==="
        TOTAL=$((TOTAL + 1))
        FAILED=$((FAILED + 1))
    fi
done

stop_daemon

echo
echo "=== E2E SUMMARY: $((TOTAL - FAILED))/$TOTAL suites passed, $FAILED failed ==="
if [[ "$FAILED" -ne 0 ]]; then
    exit 1
fi
echo "ALL E2E SUITES PASSED"
