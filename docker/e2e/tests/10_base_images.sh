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

echo "  [cleanup]"
for inst in $(andler list --json | jq -r '.[].id'); do
    andler remove "$inst" --purge >/dev/null 2>&1 || true
done
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
