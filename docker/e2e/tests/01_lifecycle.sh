#!/usr/bin/env bash
# 01 — instance lifecycle: create (file + flags), list/status (text + json),
# start/pause/resume/stop with FSM negatives, metrics, logs, remove semantics.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/01-lifecycle"
mkdir -p "$WORK"

echo "  [create]"
ID="$(create_linux "$WORK" e2e-lifecycle)"
[[ -n "$ID" ]] || fail "empty instance id from create --file"
pass "create --file returns an instance id"

expect_ok "list shows the instance" -- andler list
expect_out_grep "list shows the short id" "$ID"
expect_out_grep "list shows Created state" "Created"

expect_ok "list --json" -- andler list --json
expect_out_grep "list --json state" '"state": *"Created"'
expect_ok "list --json parses to one entry" -- jq -e 'length == 1' "$E2E_LAST_OUT"

expect_ok "status resolves the short id prefix" -- andler status "$ID"
expect_out_grep "status shows Created" "Created"

expect_ok "status --json" -- andler status "$ID" --json
expect_out_grep "status --json state" '"state": *"Created"'

expect_fail "status of a malformed instance ref" -- andler status does-not-exist
expect_err_grep "malformed ref error explains the format" "not a valid ID"
expect_fail "status of a nonexistent instance" -- andler status "$UNKNOWN_ID"
expect_err_grep "status error mentions not found" "not found"

expect_ok "config view" -- andler config view "$ID"
expect_out_grep "config shows the name" "e2e-lifecycle"
expect_out_grep "config shows headless display" "display_engine: None"

expect_fail "config of a nonexistent instance" -- andler config view "$UNKNOWN_ID"

echo "  [create negatives]"
expect_fail "create --disk-size-gib 0 rejected by clap" -- andler create --kind linux --name zero-disk --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2" --disk-size-gib 0
expect_fail "create --kind linux without --iso-path" -- andler create --kind linux --name no-iso --disk-path "$WORK/disk.qcow2"
expect_err_grep "missing iso routes to wizard guidance" "no TTY"
expect_fail "create --kind android without --android-version" -- andler create --kind android --name no-av
expect_err_grep "missing android-version error is actionable" "--android-version is required"
expect_fail "--dry-run and --quick are mutually exclusive" -- andler create --kind linux --name x --quick --dry-run --iso-path "$WORK/empty.iso" --disk-path "$WORK/disk.qcow2"

echo "  [lifecycle]"
expect_ok "start" -- andler start "$ID"
expect_ok "status after start" -- andler status "$ID"
expect_out_grep "status shows Running" "Running"

expect_fail "start while running is an FSM error" -- andler start "$ID"
expect_err_grep "start-while-running message" "not allowed"

expect_ok "pause" -- andler pause "$ID"
expect_ok "status after pause" -- andler status "$ID"
expect_out_grep "status shows Paused" "Paused"

expect_ok "pause while paused is an idempotent no-op" -- andler pause "$ID"

expect_ok "resume" -- andler resume "$ID"
expect_ok "status after resume" -- andler status "$ID"
expect_out_grep "status shows Running again" "Running"

expect_ok "resume while running is an idempotent no-op" -- andler resume "$ID"

expect_fail "guest install while running (no guest agent)" -- andler guest install qemu-guest-agent "$ID"
expect_err_grep "guest-agent-unavailable message" "guest agent"

echo "  [metrics]"
expect_ok "metrics --once returns a sample" -- timeout 10 andler metrics "$ID" --once
expect_out_grep "metrics sample has cpu" "cpu="
expect_ok "metrics --once --json" -- timeout 10 andler metrics "$ID" --once --json
expect_out_grep "metrics json has cpu_percent" "cpu_percent"
expect_ok "metrics json parses" -- jq -e 'has("cpu_percent")' "$E2E_LAST_OUT"

echo "  [logs + stop]"
LOGS_FILE="$WORK/logs-during-run.txt"
timeout 20 andler logs "$ID" >"$LOGS_FILE" 2>&1 &
LOGS_BG=$!
sleep 1

expect_ok "stop --graceful" -- andler stop "$ID" --graceful
wait "$LOGS_BG" || true
if grep -qE '^\[stderr\].*terminating on signal' "$LOGS_FILE"; then
    pass "logs captured the qemu SIGTERM line"
else
    echo "    logs content:"
    sed 's/^/      /' "$LOGS_FILE" | head -20
    fail "logs did not capture the qemu SIGTERM line"
fi

expect_ok "status after stop" -- andler status "$ID"
expect_out_grep "status shows Stopped" "Stopped"

expect_ok "logs after stop complete immediately" -- andler logs "$ID"
expect_out_nogrep "no log lines after stop" '^\[(stdout|stderr)\]'

expect_fail "stop again is rejected" -- andler stop "$ID" --graceful
expect_err_grep "double-stop message" "not found by backend"

expect_fail "pause while stopped is rejected" -- andler pause "$ID"
expect_fail "resume while stopped is rejected" -- andler resume "$ID"

echo "  [remove]"
expect_ok "remove without --purge" -- andler remove "$ID"
expect_ok "list is empty after remove" -- andler list
expect_out_grep "list says no instances" "no instances"
expect_file "disk file kept without --purge" "$WORK/disk.qcow2"

expect_fail "remove of a nonexistent instance" -- andler remove "$UNKNOWN_ID" --purge
expect_err_grep "remove error mentions not found" "not found"

ID2="$(create_linux "$WORK" e2e-purge)"
[[ -n "$ID2" ]] || fail "empty instance id from second create"
pass "second create --file works"

expect_ok "remove --purge" -- andler remove "$ID2" --purge
expect_no_file "disk.qcow2 deleted by --purge" "$WORK/disk.qcow2"
expect_file "user VARS.fd template survives --purge" "$WORK/VARS.fd"
expect_file "unrelated iso untouched by --purge" "$WORK/empty.iso"

expect_ok "list empty after purge" -- andler list
expect_out_grep "no instances" "no instances"
