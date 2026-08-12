#!/usr/bin/env bash
# 09 — daemon restart persistence and final cleanup. Restarts the shared
# daemon process (13 also restarts it and runs after this suite).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/09-persistence"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-persist)"
[[ -n "$ID" ]] || fail "empty id from create"

expect_ok "start" -- andler start "$ID"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "status shows Stopped" -- andler status "$ID"
expect_out_grep "Stopped" "Stopped"

echo "  [registry audit log]"
INSTANCE_TOML="$(echo "$HOME/.andler/instances/$ID"*/instance.toml)"
EVENTS_JSONL="$(echo "$HOME/.andler/instances/$ID"*/events.jsonl)"
[[ -n "$EVENTS_JSONL" && -f "$EVENTS_JSONL" ]] || fail "events.jsonl not found"
pass "events.jsonl exists next to instance.toml"
[[ -s "$EVENTS_JSONL" ]] || fail "events.jsonl is empty"
pass "events.jsonl records lifecycle events"
grep -qE '"kind":\{"Lifecycle":\{' "$EVENTS_JSONL" || fail "no lifecycle event in events.jsonl"
pass "events.jsonl holds lifecycle records"
grep -q '"to":"Stopped"' "$EVENTS_JSONL" || fail "no Stopped transition in events.jsonl"
pass "events.jsonl records the stop transition"

echo "  [daemon restart]"
ID_CREATED="$(create_linux "$WORK" e2e-persist-created)"
[[ -n "$ID_CREATED" ]] || fail "empty id from create"

# The broken entry must exist before the daemon starts, so the startup scan
# sees it: directories created under a live daemon are not rescanned until
# the next restart.
BROKEN_ID="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
BROKEN_DIR="$HOME/.andler/instances/$BROKEN_ID"
mkdir -p "$BROKEN_DIR"
touch "$BROKEN_DIR/disk.qcow2"

stop_daemon
start_daemon

expect_ok "instance survives the daemon restart" -- andler list
expect_out_grep "persisted id" "$ID"
expect_out_grep "persisted state is Stopped" "Stopped"
expect_out_grep "created-only instance survives" "$ID_CREATED"

expect_ok "status after the restart" -- andler status "$ID"
expect_out_grep "status Stopped" "Stopped"

expect_ok "remove --purge" -- andler remove "$ID_CREATED" --purge

echo "  [broken registry entry]"
expect_ok "list shows the broken entry" -- andler list
expect_out_grep "broken entry with reason" "\[broken:"
expect_ok "broken entry is removable" -- andler remove "$BROKEN_ID" --purge
expect_no_file "broken dir removed" "$BROKEN_DIR"
expect_ok "list clean after removing the broken entry" -- andler list
expect_out_nogrep "no broken entries left" "\[broken:"

echo "  [single-daemon flock]"
SECOND_LOG="$E2E_WORKDIR/09-second-daemon.log"
ANDLERD_STORE_PATH="$E2E_WORKDIR/09-second.db" "${ANDLERD_BIN:-/usr/local/bin/andlerd}" >>"$SECOND_LOG" 2>&1 &
SECOND_PID=$!
for _ in $(seq 1 15); do
    if ! kill -0 "$SECOND_PID" 2>/dev/null || grep -q "already running" "$SECOND_LOG"; then
        break
    fi
    sleep 0.2
done
if kill -0 "$SECOND_PID" 2>/dev/null; then
    kill "$SECOND_PID" 2>/dev/null || true
    fail "second andlerd must refuse to start while the first holds ANDLER_HOME"
else
    pass "second andlerd refused to start (flock)"
fi
if grep -q "already running" "$SECOND_LOG"; then
    pass "flock refusal message explains the conflict"
else
    sed 's/^/      /' "$SECOND_LOG" | head -10
    fail "flock refusal message missing"
fi

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"