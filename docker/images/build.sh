#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GIT_REV="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo local)"
SCRIPT_START_TS="$(date +%s)"

CLEAN_AFTER=0
ANDROID_MAJOR=""
ANDROID_VARIANT=""
VARIANT_LOWER=""
IMAGE_TAG=""
OUTPUT_QCOW2=""
DISK_SIZE=""
SUDO_KEEPALIVE_PID=""

# --- Error diagnostics --------------------------------------------------------

on_error() {
    local exit_code=$?
    echo "build.sh: error (code $exit_code) at line ${BASH_LINENO[0]}: ${BASH_COMMAND}" >&2
}
trap on_error ERR

# --- Arguments ----------------------------------------------------------------

parse_args() {
    local arg
    local args=()
    for arg in "$@"; do
        if [[ "$arg" == "--clean" ]]; then
            CLEAN_AFTER=1
        else
            args+=("$arg")
        fi
    done
    set -- "${args[@]+"${args[@]}"}"

    if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
        echo "Usage: $(basename "$0") [--clean] [android_major=13] [variant=VANILLA] [output.qcow2] [size=16G]"
        echo "  --clean  Remove intermediate docker image after build-disk.sh extracts the qcow2."
        exit 0
    fi

    ANDROID_MAJOR="${1:-13}"
    ANDROID_VARIANT="${2:-VANILLA}"

    if [[ "$ANDROID_MAJOR" != "11" && "$ANDROID_MAJOR" != "13" ]]; then
        echo "build.sh: unsupported Android version: $ANDROID_MAJOR (expected 11 or 13)" >&2
        exit 1
    fi
    if [[ "$ANDROID_VARIANT" != "VANILLA" && "$ANDROID_VARIANT" != "GAPPS" ]]; then
        echo "build.sh: unsupported variant: $ANDROID_VARIANT (expected VANILLA or GAPPS)" >&2
        exit 1
    fi

    VARIANT_LOWER="$(tr '[:upper:]' '[:lower:]' <<< "$ANDROID_VARIANT")"
    IMAGE_TAG="andler-base-rootfs:android${ANDROID_MAJOR}-${VARIANT_LOWER}-${GIT_REV}"
    # Images land in a per-version-variant subdirectory of the cache; discovery
    # scans both this layout and the legacy flat cache root.
    OUTPUT_QCOW2="${3:-$HOME/.andler/cache/base-images/android${ANDROID_MAJOR}-${VARIANT_LOWER}/linux-waydroid-android${ANDROID_MAJOR}-${VARIANT_LOWER}-${GIT_REV}.qcow2}"
    DISK_SIZE="${4:-16G}"

    if [[ "$ANDROID_MAJOR" == "11" ]]; then
        echo "==> Android 11: upstream lineage-18.1 has not been updated since 2025-06-28 — using" \
             "the last available release (expected, not an error)."
    fi
}

# --- sudo once, before long build ---------------------------------------------

# Prompt for sudo now; keepalive session for build-disk.sh later
sudo_keepalive_stop() {
    if [[ -n "$SUDO_KEEPALIVE_PID" ]]; then
        kill "$SUDO_KEEPALIVE_PID" 2>/dev/null || true
    fi
}

acquire_sudo() {
    echo "==> sudo will be needed for step 2 (disk partitioning) — asking for password now, not after the build"
    sudo -v

    trap sudo_keepalive_stop EXIT
    ( while true; do sudo -n true 2>/dev/null; sleep 60; done ) &
    SUDO_KEEPALIVE_PID=$!
}

# --- 1. docker buildx build --------------------------------------------------

# Prefer buildx; fallback to legacy docker build if unavailable
build_rootfs_image() {
    local build_cmd=(docker buildx build --load)
    if ! docker buildx version >/dev/null 2>&1; then
        echo "==> docker buildx not found — falling back to legacy docker build (slower, DEPRECATED warning)" >&2
        build_cmd=(docker build)
    fi

    echo "==> [1/2] docker build (${IMAGE_TAG}, Android ${ANDROID_MAJOR} ${ANDROID_VARIANT})"
    "${build_cmd[@]}" \
        -f "$SCRIPT_DIR/base/Dockerfile" \
        --build-arg "ANDROID_MAJOR=${ANDROID_MAJOR}" \
        --build-arg "ANDROID_VARIANT=${ANDROID_VARIANT}" \
        -t "$IMAGE_TAG" "$SCRIPT_DIR/base"
}

# --- 2. build-disk.sh ---------------------------------------------------------

build_disk_image() {
    echo "==> [2/2] build-disk.sh (sudo already confirmed in step 1, no second prompt)"
    sudo "$SCRIPT_DIR/build-disk.sh" --force "$IMAGE_TAG" "$OUTPUT_QCOW2" "$DISK_SIZE"

    sudo_keepalive_stop
    trap - EXIT
}

clean_intermediate_image() {
    [[ "$CLEAN_AFTER" -eq 1 ]] || return 0
    echo "==> --clean: removing intermediate docker image ${IMAGE_TAG}"
    docker rmi "$IMAGE_TAG" >/dev/null 2>&1 || echo "    (could not remove — image may already be absent)"
}

print_summary() {
    local elapsed=$(( $(date +%s) - SCRIPT_START_TS ))
    echo "==> base image ready: $OUTPUT_QCOW2"
    echo "    total time: $((elapsed / 60))m $((elapsed % 60))s"
}

# --- main ---------------------------------------------------------------------

main() {
    parse_args "$@"
    acquire_sudo
    build_rootfs_image
    build_disk_image
    clean_intermediate_image
    print_summary
}

main "$@"
