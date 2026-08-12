#!/usr/bin/env bash
# 14 — long-running operations: restore runs as a tracked operation (events
# on the bus and in the audit trail), `op list`/`op cancel` wire contract.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/14-op-progress"
mkdir -p "$WORK"

ID="$(create_linux "$WORK" e2e-op)"
[[ -n "$ID" ]] || fail "empty instance id from create"
INSTANCE_DIR="$(echo "$HOME/.andler/instances/$ID"*/)"

echo "  [operation audit trail]"
expect_ok "start" -- andler start "$ID"
expect_ok "snapshot create snap-1" -- andler snapshot create "$ID" --tag snap-1
expect_ok "snapshot create snap-2" -- andler snapshot create "$ID" --tag snap-2
expect_ok "stop" -- andler stop "$ID" --graceful

expect_ok "restore runs as an operation" -- andler snapshot restore "$ID" --tag snap-1

# The restore was executed as a tracked operation: the per-instance audit
# trail must contain its Running and Done events with the operation kind.
EVENTS_JSONL="$INSTANCE_DIR/events.jsonl"
expect_file "events.jsonl exists" "$EVENTS_JSONL"
expect_ok "operation events recorded" -- grep -q '"kind":{"Operation"' "$EVENTS_JSONL"
expect_ok "operation Done event recorded" -- grep -q '"state":"Done"' "$EVENTS_JSONL"
expect_ok "snapshot-restore kind recorded" -- grep -q '"kind":"SnapshotRestore"' "$EVENTS_JSONL"

echo "  [op list / op cancel contract]"
expect_ok "op list" -- andler op list
expect_out_grep "no active operations after completion" "no active operations"
expect_ok "op list --json" -- andler op --json list
expect_ok "op list json is an empty array" -- jq -e 'length == 0' <<<"$(cat "$E2E_LAST_OUT")"

expect_fail "op cancel of an unknown operation" -- andler op cancel op-unknown
expect_err_grep "op-cancel message" "no active operation"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
