#!/usr/bin/env bash
# 31 — OCI image export: create a LinuxVM, export its disk to a conformant OCI
# layout, and verify the layout (oci-layout, index.json, config.json, and a
# blob named by its sha256 digest) before cleanup. Also covers the not-found
# error path. No guest booting: the export reads the stopped instance's qcow2.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/31-oci-export"
mkdir -p "$WORK"

echo "  [linux create]"
OID="$(create_linux "$WORK" oci-linux)"
[[ -n "$OID" ]] || fail "empty id from linux create"
pass "linux instance created"

echo "  [export to OCI]"
DEST="$WORK/exported-oci"
expect_ok "export disk to OCI layout" -- andler export-oci "$OID" "$DEST" --disk-format raw
expect_file "oci-layout marker written" "$DEST/oci-layout"
expect_file "index.json written" "$DEST/index.json"
expect_file "config.json written" "$DEST/config.json"
expect_file "blobs/sha256 written" "$DEST/blobs/sha256"

echo "  [layout conformance]"
# index.json references exactly one manifest with platform arch amd64.
MANIFEST_COUNT="$(jq '.manifests | length' "$DEST/index.json")"
[[ "$MANIFEST_COUNT" == "1" ]] || fail "index.json must reference exactly one manifest"
PASS_PLATFORM="$(jq -r '.manifests[0].platform.architecture' "$DEST/index.json")"
[[ "$PASS_PLATFORM" == "amd64" ]] || fail "manifest platform arch is not amd64"
pass "index.json references one manifest with platform arch amd64"

# config.json's rootfs diff_id must name the layer blob stored under
# blobs/sha256/ — the on-disk link between the config descriptor and the layer.
DIFF_ID="$(jq -r '.rootfs.diff_ids[0]' "$DEST/config.json")"
[[ "$DIFF_ID" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "config.json diff_id malformed"
[[ -e "$DEST/blobs/sha256/${DIFF_ID#sha256:}" ]] || fail "layer blob missing from blobs/sha256"
pass "config.json diff_id resolves to the layer blob"

echo "  [export --json]"
JSON_OUT="$(andler export-oci "$OID" "$DEST" --disk-format raw --json)"
expect_ok "export-oci --json reports dest_path" -- jq -e '.dest_path == "'"$DEST"'"' <<<"$JSON_OUT"
expect_ok "export-oci --json reports source_instance_id" -- jq -e '.source_instance_id == "'"$OID"'"' <<<"$JSON_OUT"

echo "  [negatives]"
expect_fail "export of a nonexistent source fails" -- andler export-oci "$UNKNOWN_ID" "$WORK/missing" --disk-format raw
expect_err_grep "missing source reports not found" "not found"

echo "  [cleanup]"
expect_ok "remove the linux instance" -- andler remove "$OID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
