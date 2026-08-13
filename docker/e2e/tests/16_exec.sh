#!/usr/bin/env bash
# 16 — andler exec: guest-agent command execution wire contract and
# state-machine negatives (positive path needs a guest with qemu-ga,
# covered at the QMP level in unit tests).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/16-exec"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-exec)"
[[ -n "$ID" ]] || fail "empty instance id from create"

echo "  [state machine negatives]"
expect_fail "exec on a stopped instance" -- andler exec "$ID" -- echo hi
expect_err_grep "exec-while-stopped message" "requires instance"

expect_fail "exec on an unknown instance" -- andler exec "$UNKNOWN_ID" -- echo hi
expect_err_grep "unknown-instance message" "not found"

echo "  [exec on a running instance without a guest agent]"
expect_ok "start" -- andler start "$ID"
expect_fail "exec without a guest agent" -- andler exec "$ID" -- echo hi
expect_err_grep "guest-agent message" "guest agent is not available"

echo "  [cleanup]"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
