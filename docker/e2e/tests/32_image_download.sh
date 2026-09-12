#!/usr/bin/env bash
# 32 — base-image download: the daemon's release-catalog RPCs and the
# verified download/install into the local cache, driven end to end from the
# CLI against a local fixture "release server".
#
# The fixture reproduces exactly what .github/workflows/build-base-image.yml
# publishes: a GitHub releases JSON whose assets are one `<stem>.manifest.json`
# plus `<stem>.qcow2.zst.NN.part` files split out of one zstd stream.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/32-image-download"
FIXTURE="$WORK/fixture"
# GitHub's URL space, mirrored on disk: the release index, and every asset
# under the API asset path the release JSON points at.
RELEASES_DIR="$FIXTURE/repos/acme/andler"
ASSETS="$RELEASES_DIR/releases/assets"
CACHE="$HOME/.andler/cache/base-images"
mkdir -p "$ASSETS" "$RELEASES_DIR/releases"

# Fixed port: the releases JSON carries absolute asset URLs, so the fixture
# has to know its own address before anything is served.
PORT="${E2E_IMAGE_FIXTURE_PORT:-18080}"
BASE_URL="http://127.0.0.1:$PORT"

STEM13="linux-waydroid-android13-vanilla-e2e1234"
STEM11="linux-waydroid-android11-vanilla-e2e1234"
TAG13="base-image-android13-vanilla-20260901-000000"
TAG11="base-image-android11-vanilla-20260901-000000"
# Build ids (`android<major>-<variant>-<built_at>`) are what the catalog
# prints; the release tag is the selector.
ID13="android13-vanilla-2026-09-01T00:00:00Z"
ID11="android11-vanilla-2026-09-01T00:00:00Z"

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

# One real payload per Android version: a thin qcow2 compressed with zstd and
# split the way the release workflow splits it (1900M chunks there; small
# chunks here so the download exercises multiple parts quickly).
build_release() {
    local major="$1" stem="$2" tag="$3" variant="VANILLA"
    local payload="$WORK/payload-android$major.qcow2"
    qemu-img create -f qcow2 "$payload" 8M >/dev/null 2>&1
    zstd -q -19 "$payload" -o "$payload.zst"
    split -b 100k -d --additional-suffix=.part "$payload.zst" "$ASSETS/$stem.qcow2.zst."

    local parts_json=""
    local assets_json=""
    local part name
    for part in "$ASSETS/$stem.qcow2.zst."*.part; do
        name="$(basename "$part")"
        [[ -n "$parts_json" ]] && parts_json+=","
        [[ -n "$assets_json" ]] && assets_json+=","
        parts_json+="{\"name\":\"$name\",\"sha256\":\"$(sha256_of "$part")\",\"size_bytes\":$(stat -c%s "$part")}"
        assets_json+="{\"name\":\"$name\",\"url\":\"$BASE_URL/repos/acme/andler/releases/assets/$name\",\"size\":$(stat -c%s "$part")}"
    done

    local qcow_size qcow_sha
    qcow_size="$(stat -c%s "$payload")"
    qcow_sha="$(sha256_of "$payload")"

    cat > "$ASSETS/$stem.manifest.json" <<EOF
{"schema_version":1,"source_image":"andler-base:android$major-vanilla","built_at":"2026-09-01T00:00:00Z","git_rev":"e2e1234","file_size_bytes":$qcow_size,"sha256":"$qcow_sha","android_major":"$major","android_variant":"$variant","compression":"zstd","parts":[$parts_json]}
EOF

    local manifest_size
    manifest_size="$(stat -c%s "$ASSETS/$stem.manifest.json")"
    # `prerelease: true` is the shape the pipeline publishes (and `url` is the
    # API asset URL, the only fetchable one for a private repository).
    printf '{"tag_name":"%s","draft":false,"prerelease":true,"assets":[{"name":"%s.manifest.json","url":"%s/repos/acme/andler/releases/assets/%s.manifest.json","size":%s}%s]}' \
        "$tag" "$stem" "$BASE_URL" "$stem" "$manifest_size" \
        "$([[ -n "$assets_json" ]] && echo ",$assets_json")"
}

echo "  [fixture release server]"
build_release 13 "$STEM13" "$TAG13" > "$WORK/release13.json"
build_release 11 "$STEM11" "$TAG11" > "$WORK/release11.json"
# A draft release must never be offered: it is what the workflow publishes
# while the image is still being uploaded.
printf '{"tag_name":"base-image-android13-gapps-20260902-000000","draft":true,"prerelease":false,"assets":[]}' > "$WORK/draft.json"
# `releases` is a directory here (the asset tree lives under
# `releases/assets/...`), and python's http.server answers a directory request
# with its `index.html` — which is exactly the URL shape GitHub serves.
{ printf '['; cat "$WORK/release13.json"; printf ','; cat "$WORK/release11.json"; printf ','; cat "$WORK/draft.json"; printf ']\n'; } \
    > "$RELEASES_DIR/releases/index.html"

python3 -m http.server "$PORT" --directory "$FIXTURE" >"$WORK/http.log" 2>&1 &
HTTP_PID=$!
for _ in $(seq 1 40); do
    curl -fsS "$BASE_URL/repos/acme/andler/releases" >/dev/null 2>&1 && break
    sleep 0.25
done
curl -fsS "$BASE_URL/repos/acme/andler/releases" >/dev/null \
    || fail "fixture release server did not come up on port $PORT"
pass "fixture release server serving two releases"

cleanup() {
    kill "$HTTP_PID" 2>/dev/null || true
    rm -rf "$CACHE/android13-vanilla" "$CACHE/android11-vanilla"
    echo "  asserts: $E2E_PASS passed, $E2E_FAIL failed"
}
trap cleanup EXIT

# The daemon reads its catalog source from the environment at call time; the
# harness passes its own environment on to the daemon, so a restart picks the
# fixture up (same pattern as the persistence/autostart suites).
export ANDLERD_IMAGE_REPO="acme/andler"
export ANDLERD_IMAGE_API_BASE="$BASE_URL"
# A configured credential must not change anything a static fixture can see —
# the fixture both ignores it and (in the unit tests) asserts it is sent.
export ANDLERD_IMAGE_TOKEN="e2e-fixture-token"
stop_daemon
start_daemon

echo "  [image list: published catalog]"
expect_ok "list published images" -- andler image list
expect_out_grep "source names the repository" "acme/andler"
expect_out_grep "android 13 build is offered" "$ID13"
expect_out_grep "android 11 build is offered" "$ID11"
expect_out_grep "nothing is cached yet" "not cached"

# One capture, several assertions: each `expect_ok` overwrites $E2E_LAST_OUT,
# so the JSON is snapshotted once and the jq checks read the file.
expect_ok "list --json" -- andler image list --json
cp "$E2E_LAST_OUT" "$WORK/list.json"
expect_ok "json reports the source" -- jq -e '.source | contains("acme/andler")' "$WORK/list.json"
expect_ok "json reports both builds" -- jq -e '[.images[].android_major] | sort == ["11","13"]' "$WORK/list.json"
expect_ok "json reports the parts total as the download size" -- \
    jq -e '.images[] | select(.android_major == "13") | .download_bytes > 0' "$WORK/list.json"
expect_ok "json reports nothing installed" -- jq -e '[.images[].installed] | all(. == false)' "$WORK/list.json"
expect_ok "the draft release is not offered" -- jq -e '[.images[].release_tag] | all(contains("gapps") | not)' "$WORK/list.json"
expect_ok "prerelease builds are offered" -- jq -e '[.images[].release_tag] | length == 2' "$WORK/list.json"

expect_ok "filter by android version" -- andler image list --android-version 11
expect_out_grep "android 11 build listed" "$ID11"
expect_out_nogrep "android 13 build filtered out" "$ID13"

echo "  [image download: verified install]"
expect_ok "download the android 13 build" -- timeout 300 andler image download --android-version 13 --variant vanilla
INSTALLED_13="$CACHE/android13-vanilla/$STEM13.qcow2"
expect_file "image installed in the cache" "$INSTALLED_13"
expect_file "published manifest kept next to it" "$CACHE/android13-vanilla/$STEM13.manifest.json"
cmp -s "$INSTALLED_13" "$WORK/payload-android13.qcow2" \
    && pass "installed image matches the published payload byte for byte" \
    || fail "installed image differs from the published payload"
expect_ok "manifest provenance kept verbatim" -- \
    jq -e '.android_variant == "VANILLA" and .compression == "zstd" and (.parts | length) >= 1' \
    "$CACHE/android13-vanilla/$STEM13.manifest.json"
ls "$CACHE/android13-vanilla"/.fetch-* >/dev/null 2>&1 \
    && fail "download scratch directory was left behind" \
    || pass "download scratch directory removed"

expect_ok "re-downloading the same build is a no-op" -- andler image download --android-version 13 --variant vanilla
expect_out_grep "reuse is reported" "already cached"

# The catalog walk is cached for five minutes: a second listing is served from
# it, so `image list` + `image download` do not spend the request budget twice.
# (The fixture cannot count requests here — python's http.server has no hook —
# so this asserts the user-visible half: the catalog is still complete.)
expect_ok "listing again still reports the full catalog" -- andler image list
expect_out_grep "android 13 still listed" "$ID13"
expect_out_grep "android 11 still listed" "$ID11"

expect_ok "list reports the cached build" -- andler image list
expect_out_grep "android 13 build is cached" "cached: $INSTALLED_13"
expect_ok "list --json reports it as installed" -- andler image list --json
cp "$E2E_LAST_OUT" "$WORK/list-after.json"
expect_ok "installed flag is true" -- \
    jq -e '.images[] | select(.android_major == "13") | .installed == true' "$WORK/list-after.json"

expect_ok "download by release tag" -- timeout 300 andler image download --release-tag "$TAG11"
expect_file "tag-selected build installed" "$CACHE/android11-vanilla/$STEM11.qcow2"

echo "  [image download: corrupt release data is refused]"
# Corrupt one part on the wire (the fixture serves straight off disk) and
# force a re-download: the sha256 in the manifest must stop the install.
CORRUPT_TARGET="$(ls "$ASSETS/$STEM13.qcow2.zst."*.part | head -n1)"
cp "$CORRUPT_TARGET" "$WORK/part-backup"
printf 'corrupt' | dd of="$CORRUPT_TARGET" bs=1 seek=0 conv=notrunc status=none

expect_fail "force re-download of a corrupted part fails" -- \
    timeout 300 andler image download --android-version 13 --variant vanilla --force
expect_err_grep "failure names the verification" "does not match its manifest sha256"
cmp -s "$INSTALLED_13" "$WORK/payload-android13.qcow2" \
    && pass "the already-installed image survived the failed re-download" \
    || fail "a failed re-download must not damage the cached image"
cp "$WORK/part-backup" "$CORRUPT_TARGET"

echo "  [image download: json progress]"
expect_ok "download reports progress as json lines" -- timeout 300 andler image download --release-tag "$TAG13" --force --json
expect_out_grep "json reports the resolving phase" '"phase":"resolving"'
expect_out_grep "json reports the download phase" '"phase":"downloading"'
expect_out_grep "json reports the done phase" '"phase":"done"'
expect_out_grep "json reports where it installed" "\"installed_path\":\"$INSTALLED_13\""

echo "  [image download: an unpublished combination]"
expect_ok "listing a combination the pipeline never published" -- andler image list --android-version 13 --variant gapps
expect_out_grep "empty catalog is explained" "No published base images matched"
expect_fail "downloading it fails with an actionable message" -- \
    andler image download --android-version 13 --variant gapps
expect_err_grep "error points at the local build path" "docker/images/build.sh"
expect_err_grep "error points at the catalog command" "andler image list"

echo "  [a downloaded image is usable]"
OVMF_TEMPLATE="$WORK/OVMF_VARS.template.fd"
qemu-img create -f raw "$OVMF_TEMPLATE" 4M >/dev/null 2>&1
expect_ok "create an instance from the downloaded image" -- \
    andler create \
    --kind android \
    --name e2e-downloaded-image \
    --android-version 13 \
    --base-image-path "$INSTALLED_13" \
    --instances-root "$WORK/instances" \
    --ovmf-vars-template "$OVMF_TEMPLATE" \
    --linked-overlay \
    --quick
ID="$(andler list --json | jq -r '.[0].id')"
[[ -n "$ID" ]] || fail "empty instance id from list"
expect_ok "the instance records the downloaded image" -- andler config view "$ID"
expect_out_grep "config points at the cached image" "android13-vanilla/$STEM13.qcow2"
expect_ok "remove the instance" -- andler remove "$ID" --purge

# Drop the fixture environment again: later suites must see the real defaults.
unset ANDLERD_IMAGE_REPO ANDLERD_IMAGE_API_BASE ANDLERD_IMAGE_TOKEN
stop_daemon
start_daemon
