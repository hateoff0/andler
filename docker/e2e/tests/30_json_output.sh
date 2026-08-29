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
echo "  [success path: doctor --json emits a JSON document]"
# doctor exits non-zero when a check warns (no KVM / base images in the
# container); capture the exit code and accept the expected 1, then assert
# the emitted document is a JSON object with overall + checks keys.
DOCTOR_RC=0
andler doctor --json >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR" || DOCTOR_RC=$?
[ "$DOCTOR_RC" -le 1 ] || fail "doctor --json exited $DOCTOR_RC"
expect_ok "doctor --json carries overall + checks" \
  -- jq -e 'has("overall") and has("checks")' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [success path: lifecycle transitions emit JSON]"
expect_ok "start --json" -- andler start "$ID" --json
expect_ok "start --json carries Running state" \
  -- jq -e '.instance_id == "'"$ID"'" and .state == "Running"' <<<"$(cat "$E2E_LAST_OUT")"
expect_ok "pause --json" -- andler pause "$ID" --json
expect_ok "pause --json carries Paused state" \
  -- jq -e '.state == "Paused"' <<<"$(cat "$E2E_LAST_OUT")"
expect_ok "resume --json" -- andler resume "$ID" --json
expect_ok "resume --json carries Running state" \
  -- jq -e '.state == "Running"' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [success path: hotplug attach emits JSON]"
andler attach --json disk "$ID" --size 2G >"$WORK/attach.json" 2>"$WORK/attach.err"
expect_ok "attach disk --json carries action + path" \
  -- jq -e '.action == "attach_disk" and .instance_id == "'"$ID"'" and (.path | type == "string")' "$WORK/attach.json"
ATTACH_PATH="$(jq -r '.path' "$WORK/attach.json")"
[[ -n "$ATTACH_PATH" && "$ATTACH_PATH" != "null" ]] || fail "attach --json missing path"
echo "  [success path: stop emits JSON]"
expect_ok "stop --json" -- andler stop "$ID" --json
expect_ok "stop --json carries Stopped state" \
  -- jq -e '.state == "Stopped"' <<<"$(cat "$E2E_LAST_OUT")"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
