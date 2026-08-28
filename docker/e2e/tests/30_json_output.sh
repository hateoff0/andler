#!/usr/bin/env bash
# 30 — --json output contract: successful commands emit a JSON document to
# stdout, and error paths emit a {"error": ...} JSON document to stderr
# (never a bare text line). This pins the unified JSON error refactor.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/30-json"
mkdir -p "$WORK"

echo "  [success path: status --json emits JSON to stdout]"
ID="$(create_linux "$WORK/ok" e2e-json-vm)"
[[ -n "$ID" ]] || fail "empty id from create"
expect_ok "status --json" -- andler status "$ID" --json
expect_ok "status --json carries instance_id and state" \
  -- jq -e '.instance_id == "'"$ID"'" and .state == "Created"' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [error path: unknown instance id emits JSON to stderr]"
expect_fail "status --json with unknown id fails" -- andler status "$UNKNOWN_ID" --json
expect_ok "status --json error is a JSON document" \
  -- jq -e 'has("error") and (.error | type == "string" and length > 0)' <<<"$(cat "$E2E_LAST_ERR")"

echo "  [error path: create validation failure emits JSON to stderr]"
expect_fail "create --json with missing ISO fails" \
  -- andler create --json --kind linux --name bad-iso \
      --iso-path "$WORK/does-not-exist.iso" \
      --disk-path "$WORK/disk.qcow2" \
      --instances-root "$WORK/instances"
expect_ok "create --json error is a JSON document" \
  -- jq -e 'has("error") and (.error | type == "string" and length > 0)' <<<"$(cat "$E2E_LAST_ERR")"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
