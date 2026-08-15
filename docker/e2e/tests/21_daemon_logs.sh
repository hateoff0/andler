#!/usr/bin/env bash
# andler logs daemon: snapshot ring, --since filter, --follow liveness.
set -u
E2E_ROOT="${E2E_ROOT:-/usr/local/share/andler-e2e}"
# shellcheck source=common.sh
source "$E2E_ROOT/tests/common.sh"

WORK="$E2E_WORKDIR/21-daemon-logs"
mkdir -p "$WORK"

echo "  [snapshot ring]"
expect_ok "logs daemon returns the startup line" -- timeout 10 andler logs daemon
expect_out_grep "startup line present" "andlerd started"

echo "  [--since filter]"
NOW_MS=$(date +%s%N | cut -c1-13)
LATER_MS=$((NOW_MS + 10000))
expect_ok "logs daemon --since in the future is empty" -- timeout 10 andler logs daemon --since "$LATER_MS"
expect_out_nogrep "no lines after a future --since" "."

echo "  [--follow stays alive]"
if timeout 3 andler logs daemon --follow >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR"; then
    fail "--follow must not terminate on its own (it streams)"
else
    rc=$?
    if [[ $rc -eq 124 ]]; then
        pass "--follow streamed until the timeout (exit 124)"
    else
        fail "--follow exited with $rc (expected 124 timeout)"
    fi
fi

echo "  [--json shape]"
expect_ok "logs daemon --json emits JSON lines" -- timeout 10 andler logs daemon --json
expect_out_grep "json has ts_ms and line" '"ts_ms":'

echo "  [cleanup]"
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
