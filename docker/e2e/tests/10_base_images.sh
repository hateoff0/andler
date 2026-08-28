#!/usr/bin/env bash
# 10 — base images: auto-discovery from the per-version-variant cache
# subdirectory layout (cache/base-images/<android_major>-<variant>/), with
# the legacy flat-root layout still working.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/10-base-images"
mkdir -p "$WORK"

# The daemon resolves an omitted --base-image-path against its own
# ANDLER_HOME (default $HOME/.andler) — seed that cache with fake images
# (real thin qcow2 files + the exact manifest shape build-disk.sh writes).
# built_at 2099 guarantees these win freshness over any real images.
CACHE="$HOME/.andler/cache/base-images"
SUBDIR="$CACHE/android13-vanilla"
mkdir -p "$SUBDIR"

qemu-img create -f qcow2 "$SUBDIR/linux-waydroid-android13-vanilla-e2e.qcow2" 1G >/dev/null 2>&1
cat > "$SUBDIR/linux-waydroid-android13-vanilla-e2e.manifest.json" <<EOF
{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2099-06-01T00:00:00Z"}
EOF

OVMF_TEMPLATE="$WORK/OVMF_VARS.template.fd"
qemu-img create -f raw "$OVMF_TEMPLATE" 4M >/dev/null 2>&1

echo "  [auto-resolve from subdirectory]"
expect_ok "create android without --base-image-path resolves from subdir" -- \
    andler create \
    --kind android \
    --name e2e-subdir-base \
    --android-version 13 \
    --instances-root "$WORK/instances" \
    --ovmf-vars-template "$OVMF_TEMPLATE" \
    --linked-overlay \
    --quick
ID="$(andler list --json | jq -r '.[0].id')"
[[ -n "$ID" ]] || fail "empty instance id from list"

expect_ok "config view shows the resolved base image" -- andler config view "$ID"
expect_out_grep "resolved image lives in the subdir" "android13-vanilla/linux-waydroid-android13-vanilla-e2e.qcow2"

echo "  [base-image pin is recorded and enforced]"
INST_TOML="$(ls "$WORK/instances/"*/instance.toml | head -1)"
[[ -n "$INST_TOML" ]] || fail "instance.toml not found under $WORK/instances/"
grep -q 'base_image_pin' "$INST_TOML" && pass "pin recorded in instance.toml" \
    || fail "base_image_pin missing from instance.toml"
grep -q 'sha256 = ".\{64\}"' "$INST_TOML" && pass "pin carries a sha256" \
    || fail "pin sha256 missing or malformed"

cat > "$WORK/pin-checksum.toml" <<EOF
name = "pin-checksum"
android_version = 13
base_image_path = "$SUBDIR/linux-waydroid-android13-vanilla-e2e.qcow2"
ovmf_vars_path = "$OVMF_TEMPLATE"
base_image_pin = { id = "android13-vanilla-2099-06-01T00:00:00Z", sha256 = "0000000000000000000000000000000000000000000000000000000000000000" }
EOF
expect_fail "create with a corrupted pin is refused" -- andler create --file "$WORK/pin-checksum.toml"
expect_err_grep "pin error mentions the checksum" "changed since it was pinned"

cat > "$WORK/pin-swap.toml" <<EOF
name = "pin-swap"
android_version = 13
base_image_path = "$SUBDIR/linux-waydroid-android13-vanilla-e2e.qcow2"
ovmf_vars_path = "$OVMF_TEMPLATE"
base_image_pin = { id = "android11-vanilla-2099-01-01T00:00:00Z", sha256 = "1111111111111111111111111111111111111111111111111111111111111111" }
EOF
expect_fail "create pinning a different image is refused" -- andler create --file "$WORK/pin-swap.toml"
expect_err_grep "pin error mentions the swap" "swapped"

echo "  [legacy flat layout still resolves]"
qemu-img create -f qcow2 "$CACHE/linux-waydroid-android11-vanilla-legacy.qcow2" 1G >/dev/null 2>&1
cat > "$CACHE/linux-waydroid-android11-vanilla-legacy.manifest.json" <<EOF
{"schema_version":1,"android_major":"11","android_variant":"VANILLA","built_at":"2099-01-01T00:00:00Z"}
EOF

expect_ok "create android 11 without --base-image-path resolves from flat root" -- \
    andler create \
    --kind android \
    --name e2e-flat-base \
    --android-version 11 \
    --instances-root "$WORK/instances" \
    --ovmf-vars-template "$OVMF_TEMPLATE" \
    --linked-overlay \
    --quick

echo "  [no matching image -> actionable failure]"
expect_fail "create android 13 GAPPS without image fails" -- \
    andler create \
    --kind android \
    --name e2e-nope \
    --android-version 13 \
    --gapps \
    --instances-root "$WORK/instances" \
    --ovmf-vars-template "$OVMF_TEMPLATE" \
    --linked-overlay \
    --quick
expect_err_grep "failure points at build.sh" "build.sh"

echo "  [cache list / clean]"
CACHE_HOME="$WORK/cache-home"
mkdir -p "$CACHE_HOME/cache/base-images"
CB="$CACHE_HOME/cache/base-images"

# android13-vanilla: two builds -> the older is superseded
qemu-img create -f qcow2 "$CB/android13-vanilla-20240101T000000Z.qcow2" 1G >/dev/null 2>&1
printf '{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2024-01-01T00:00:00Z"}' > "$CB/android13-vanilla-20240101T000000Z.manifest.json"
qemu-img create -f qcow2 "$CB/android13-vanilla-20240102T000000Z.qcow2" 1G >/dev/null 2>&1
printf '{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2024-01-02T00:00:00Z"}' > "$CB/android13-vanilla-20240102T000000Z.manifest.json"
# android11-vanilla: single build -> always kept
qemu-img create -f qcow2 "$CB/android11-vanilla-20240101T000000Z.qcow2" 1G >/dev/null 2>&1
printf '{"schema_version":1,"android_major":"11","android_variant":"VANILLA","built_at":"2024-01-01T00:00:00Z"}' > "$CB/android11-vanilla-20240101T000000Z.manifest.json"
# orphan files: a manifest with no qcow2, and a qcow2 with no manifest
printf '{"schema_version":1,"android_major":"14","android_variant":"VANILLA","built_at":"2024-01-01T00:00:00Z"}' > "$CB/android14-vanilla-20240101T000000Z.manifest.json"
qemu-img create -f qcow2 "$CB/orphan-20240101T000000Z.qcow2" 1G >/dev/null 2>&1

export ANDLER_HOME="$CACHE_HOME"

expect_ok "cache list shows all complete builds" -- andler cache list
expect_out_grep "list shows the freshest android13" "android13-vanilla-20240102T000000Z.qcow2"
expect_out_grep "list shows the superseded android13" "android13-vanilla-2024-01-01T00:00:00Z"

expect_ok "cache clean --dry-run reports without deleting" -- andler cache clean --dry-run
expect_out_grep "dry-run lists the superseded build" "android13-vanilla-2024-01-01T00:00:00Z"
expect_file "dry-run kept the superseded qcow2" "$CB/android13-vanilla-20240101T000000Z.qcow2"
expect_file "dry-run kept the orphan manifest" "$CB/android14-vanilla-20240101T000000Z.manifest.json"

expect_ok "cache clean removes superseded builds and orphans" -- andler cache clean
expect_no_file "superseded qcow2 removed" "$CB/android13-vanilla-20240101T000000Z.qcow2"
expect_no_file "superseded manifest removed" "$CB/android13-vanilla-20240101T000000Z.manifest.json"
expect_no_file "orphan manifest removed" "$CB/android14-vanilla-20240101T000000Z.manifest.json"
expect_no_file "orphan qcow2 removed" "$CB/orphan-20240101T000000Z.qcow2"
expect_file "freshest android13 kept" "$CB/android13-vanilla-20240102T000000Z.qcow2"
expect_file "android11 kept" "$CB/android11-vanilla-20240101T000000Z.qcow2"

expect_ok "cache clean is idempotent once up to date" -- andler cache clean
expect_out_grep "up to date message" "up to date"

expect_ok "cache list --json emits valid JSON" -- andler cache list --json
expect_out_grep "json includes the kept android13 build" "android13-vanilla-20240102T000000Z.qcow2"

rm -rf "$CACHE_HOME"
echo "  [cleanup]"
for inst in $(andler list --json | jq -r '.[].id'); do
    andler remove "$inst" --purge >/dev/null 2>&1 || true
done
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
