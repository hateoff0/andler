#!/usr/bin/env bash

set -euo pipefail

# --- Arguments and pre-checks ------------------------------------------------

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    cat <<EOF
Usage: $(basename "$0") [--force] <docker-image-tag> <output.qcow2> [size=16G]

Converts a container rootfs (built via base/Dockerfile) into a UEFI-bootable
GPT disk image (qcow2). Requires root for loop devices, mount, sgdisk/mkfs.

Options:
  --force   Overwrite existing output file without prompting

Example:
  docker buildx build --load -f docker/images/base/Dockerfile \\
      -t andler-base-rootfs:android13-vanilla docker/images/base
  sudo docker/images/build-disk.sh andler-base-rootfs:android13-vanilla \\
      ~/.andler/cache/base-images/linux-waydroid-android13-vanilla.qcow2 16G

Disk layout:
  /dev/vda1  ESP     512M  vfat   label ANDLER-ESP   -> /efi
  /dev/vda2  root    rest  ext4   label andler-root  -> /
EOF
    exit 0
fi

if [[ $EUID -ne 0 ]]; then
    echo "build-disk.sh: root required (loop devices, mount, sgdisk/mkfs)." >&2
    echo "Run via sudo." >&2
    exit 1
fi

# Restore ownership to original sudo user at the end
ORIGINAL_UID="${SUDO_UID:-}"
ORIGINAL_GID="${SUDO_GID:-}"
if [[ -z "$ORIGINAL_UID" || -z "$ORIGINAL_GID" ]]; then
    echo "build-disk.sh: warning — not run via sudo (SUDO_UID/SUDO_GID not set)," >&2
    echo "  output file ownership will not be corrected automatically." >&2
fi

# Extract --force before positional arg parsing
FORCE_OVERWRITE=0
ARGS=()
for arg in "$@"; do
    if [[ "$arg" == "--force" ]]; then
        FORCE_OVERWRITE=1
    else
        ARGS+=("$arg")
    fi
done
set -- "${ARGS[@]+"${ARGS[@]}"}"

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo "Usage: $0 [--force] <docker-image-tag> <output.qcow2> [size=16G]" >&2
    exit 1
fi

IMAGE_TAG="$1"
OUTPUT_QCOW2="$2"
DISK_SIZE="${3:-16G}"
ESP_SIZE_MIB=512
MIN_DISK_SIZE_MIB=3072
SCRIPT_START_TS="$(date +%s)"

# --- Error diagnostics -------------------------------------------------------

on_error() {
    local exit_code=$?
    echo "build-disk.sh: error (code $exit_code) at line ${BASH_LINENO[0]}: ${BASH_COMMAND}" >&2
}
trap on_error ERR

# --- Validate arguments ------------------------------------------------------

validate_disk_size() {
    # Reject disks smaller than ~3GB (VANILLA needs ~2-2.5GB)
    local disk_size_mib
    disk_size_mib="$(numfmt --from=iec "${DISK_SIZE%B}" 2>/dev/null)" || true
    if [[ -n "$disk_size_mib" ]]; then
        disk_size_mib="$((disk_size_mib / 1024 / 1024))"
        if [[ "$disk_size_mib" -lt "$MIN_DISK_SIZE_MIB" ]]; then
            echo "build-disk.sh: disk size ${DISK_SIZE} too small (needs ~3G minimum for VANILLA, more for GAPPS)" >&2
            exit 1
        fi
    fi
}

validate_output_path() {
    # Require .qcow2 extension; block overwriting without --force
    case "$OUTPUT_QCOW2" in
        *.qcow2) ;;
        *)
            echo "build-disk.sh: output path must end with .qcow2: $OUTPUT_QCOW2" >&2
            exit 1
            ;;
    esac

    if [[ -e "$OUTPUT_QCOW2" && "$FORCE_OVERWRITE" -ne 1 ]]; then
        echo "build-disk.sh: file already exists: $OUTPUT_QCOW2" >&2
        echo "  Script runs as root and would overwrite without warning." >&2
        echo "  If intentional (rebuilding same image), add --force." >&2
        exit 1
    fi
}

validate_image_exists() {
    # Fail early if docker image tag doesn't exist
    if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
        echo "build-disk.sh: docker image not found: $IMAGE_TAG" >&2
        echo "  Build it first: docker buildx build --load -t $IMAGE_TAG ..." >&2
        exit 1
    fi
}

# --- Check host tools --------------------------------------------------------

# Package name hints per distro for missing tools
declare -A TOOL_PACKAGE_HINT=(
    [docker]="docker.io / docker-ce (see https://docs.docker.com/engine/install/)"
    [sgdisk]="Debian/Ubuntu: apt install gdisk | Fedora: dnf install gdisk | Arch: pacman -S gptfdisk"
    [losetup]="usually part of util-linux — Debian/Ubuntu: apt install util-linux"
    [mkfs.vfat]="Debian/Ubuntu: apt install dosfstools | Fedora: dnf install dosfstools | Arch: pacman -S dosfstools"
    [mkfs.ext4]="usually part of e2fsprogs — Debian/Ubuntu: apt install e2fsprogs | Arch: pacman -S e2fsprogs"
    [qemu-img]="Debian/Ubuntu: apt install qemu-utils | Fedora: dnf install qemu-img | Arch: pacman -S qemu-img"
    [tar]="usually pre-installed"
    [python3]="Debian/Ubuntu: apt install python3 | Fedora: dnf install python3 | Arch: pacman -S python"
)

MKFS_VFAT_BIN=""

check_dependencies() {
    local bin
    for bin in docker sgdisk losetup mkfs.ext4 qemu-img tar python3; do
        command -v "$bin" >/dev/null 2>&1 || {
            echo "build-disk.sh: missing required tool: $bin" >&2
            echo "  install: ${TOOL_PACKAGE_HINT[$bin]}" >&2
            exit 1
        }
    done

    # Try mkfs.vfat first, fall back to mkfs.fat (same package, different names)
    local candidate
    for candidate in mkfs.vfat mkfs.fat; do
        if command -v "$candidate" >/dev/null 2>&1; then
            MKFS_VFAT_BIN="$candidate"
            break
        fi
    done
    if [[ -z "$MKFS_VFAT_BIN" ]]; then
        echo "build-disk.sh: neither mkfs.vfat nor mkfs.fat found" >&2
        echo "  install: ${TOOL_PACKAGE_HINT[mkfs.vfat]}" >&2
        exit 1
    fi
}

# --- Working directory and cleanup -------------------------------------------

WORKDIR=""
MNT=""
RAW_IMG=""
ROOTFS_TAR=""
CONTAINER_ID=""
LOOP_DEV=""

cleanup() {
    local ec=$?
    set +e
    if [[ -n "$LOOP_DEV" ]]; then
        mountpoint -q "$MNT/efi" && umount "$MNT/efi"
        mountpoint -q "$MNT" && umount "$MNT"
        losetup -d "$LOOP_DEV" 2>/dev/null
    fi
    if [[ -n "$CONTAINER_ID" ]]; then
        docker rm -f "$CONTAINER_ID" >/dev/null 2>&1
    fi
    [[ -n "$WORKDIR" ]] && rm -rf "$WORKDIR"
    exit "$ec"
}

setup_workdir() {
    WORKDIR="$(mktemp -d /tmp/andler-build-disk.XXXXXX)"
    MNT="$WORKDIR/mnt"
    RAW_IMG="$WORKDIR/disk.raw"
    ROOTFS_TAR="$WORKDIR/rootfs.tar"
    trap cleanup EXIT INT TERM
    mkdir -p "$MNT"
    mkdir -p "$(dirname "$OUTPUT_QCOW2")"
}

# --- 1. Export rootfs from docker image --------------------------------------

export_rootfs() {
    echo "==> Exporting rootfs from ${IMAGE_TAG}"
    CONTAINER_ID="$(docker create "$IMAGE_TAG")"
    docker export "$CONTAINER_ID" -o "$ROOTFS_TAR"
    docker rm -f "$CONTAINER_ID" >/dev/null
    CONTAINER_ID=""
}

# --- 2. Empty raw disk and GPT partitioning -----------------------------------

create_disk_image() {
    echo "==> Creating disk (${DISK_SIZE}) with GPT partitioning"
    qemu-img create -f raw "$RAW_IMG" "$DISK_SIZE"

    sgdisk -Z "$RAW_IMG"
    sgdisk \
        -n 1:0:+"${ESP_SIZE_MIB}M" -t 1:ef00 -c 1:ANDLER-ESP \
        -n 2:0:0 -t 2:8300 -c 2:andler-root \
        "$RAW_IMG"
}

# --- 3. Loop device and filesystems ------------------------------------------

wait_for_partitions() {
    # Kernel may need a moment after partprobe to create /dev/loopNpM nodes
    udevadm settle --timeout=5 2>/dev/null || true
    local wait_iterations=0
    while [[ ! -e "${LOOP_DEV}p1" || ! -e "${LOOP_DEV}p2" ]]; do
        if [[ "$wait_iterations" -ge 20 ]]; then
            echo "build-disk.sh: ${LOOP_DEV}p1/p2 not found within 10s after partitioning" >&2
            exit 1
        fi
        sleep 0.5
        wait_iterations="$((wait_iterations + 1))"
    done
}

format_partitions() {
    LOOP_DEV="$(losetup -fP --show "$RAW_IMG")"
    echo "==> Loop device: $LOOP_DEV"

    wait_for_partitions

    "$MKFS_VFAT_BIN" -F32 -n ANDLER-ESP "${LOOP_DEV}p1"
    mkfs.ext4 -q -L andler-root "${LOOP_DEV}p2"
}

# --- 4. Mount and extract ----------------------------------------------------

extract_rootfs() {
    # Mount ESP before rootfs extraction so tar places UKI on real vfat
    mount "${LOOP_DEV}p2" "$MNT"
    mkdir -p "$MNT/efi"
    mount "${LOOP_DEV}p1" "$MNT/efi"

    echo "==> Extracting rootfs to disk"
    # --acls omitted: vfat doesn't support POSIX ACLs
    tar -xpf "$ROOTFS_TAR" -C "$MNT" --numeric-owner --xattrs

    sync

    umount "$MNT/efi"
    umount "$MNT"
    losetup -d "$LOOP_DEV"
    LOOP_DEV=""
}

# --- 5. Convert to qcow2 ----------------------------------------------------

convert_to_qcow2() {
    echo "==> Converting to qcow2: $OUTPUT_QCOW2"
    qemu-img convert -O qcow2 -c "$RAW_IMG" "$OUTPUT_QCOW2"
}

# --- 6. Manifest -------------------------------------------------------------

write_manifest() {
    local sha256 size_bytes size_human build_date git_rev android_major android_variant
    local android_receipt_json manifest

    sha256="$(sha256sum "$OUTPUT_QCOW2" | cut -d' ' -f1)"
    size_bytes="$(stat -c%s "$OUTPUT_QCOW2" 2>/dev/null || stat -f%z "$OUTPUT_QCOW2")"
    size_human="$(du -h "$OUTPUT_QCOW2" | cut -f1)"
    build_date="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    git_rev="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    android_major="$(docker inspect -f '{{ index .Config.Labels "org.andler.image.android_major" }}' "$IMAGE_TAG" 2>/dev/null || echo unknown)"
    android_variant="$(docker inspect -f '{{ index .Config.Labels "org.andler.image.android_variant" }}' "$IMAGE_TAG" 2>/dev/null || echo unknown)"

    # Read fetch manifest from rootfs tar (written during docker build)
    android_receipt_json="{}"
    if tar -tf "$ROOTFS_TAR" | grep -q '^etc/waydroid-extra/images/andler-fetch-manifest.json$' 2>/dev/null; then
        android_receipt_json="$(tar -xO -f "$ROOTFS_TAR" etc/waydroid-extra/images/andler-fetch-manifest.json 2>/dev/null || echo '{}')"
    fi

    manifest="${OUTPUT_QCOW2%.qcow2}.manifest.json"
    python3 - "$manifest" <<PYEOF
import json, sys

manifest = {
    "schema_version": 1,
    "source_image": "${IMAGE_TAG}",
    "built_at": "${build_date}",
    "git_rev": "${git_rev}",
    "disk_size": "${DISK_SIZE}",
    "file_size_bytes": ${size_bytes},
    "file_size_human": "${size_human}",
    "sha256": "${sha256}",
    "android_major": "${android_major}",
    "android_variant": "${android_variant}",
    "android_images": json.loads('''${android_receipt_json}'''),
    "partitions": [
        {"label": "ANDLER-ESP", "fs": "vfat", "size_mib": ${ESP_SIZE_MIB}, "mount": "/efi"},
        {"label": "andler-root", "fs": "ext4", "mount": "/"},
    ],
}
with open(sys.argv[1], "w") as f:
    json.dump(manifest, f, indent=2, ensure_ascii=False)
    f.write("\n")
PYEOF

    # Global results for print_summary() (intentionally not local)
    RESULT_SHA256="$sha256"
    RESULT_SIZE_HUMAN="$size_human"
    RESULT_SIZE_BYTES="$size_bytes"
    RESULT_MANIFEST="$manifest"
}

# --- 7. Restore ownership to original user -----------------------------------

fix_ownership() {
    # chown output dir and parent dirs back to original sudo user
    [[ -n "$ORIGINAL_UID" && -n "$ORIGINAL_GID" ]] || return 0

    local output_dir dir
    output_dir="$(dirname "$OUTPUT_QCOW2")"

    chown -R "${ORIGINAL_UID}:${ORIGINAL_GID}" "$output_dir"

    dir="$output_dir"
    while [[ "$dir" != "$HOME" && "$dir" != "/" && -n "$dir" ]]; do
        chown "${ORIGINAL_UID}:${ORIGINAL_GID}" "$dir" 2>/dev/null || true
        dir="$(dirname "$dir")"
    done
}

# --- 8. Summary ---------------------------------------------------------------

print_summary() {
    local elapsed=$(( $(date +%s) - SCRIPT_START_TS ))
    echo "==> Done: $OUTPUT_QCOW2"
    echo "    size: $RESULT_SIZE_HUMAN ($RESULT_SIZE_BYTES bytes)"
    echo "    sha256: $RESULT_SHA256"
    echo "    manifest: $RESULT_MANIFEST"
    echo "    time: $((elapsed / 60))m $((elapsed % 60))s"
}

# --- main ---------------------------------------------------------------------

main() {
    validate_disk_size
    validate_output_path
    check_dependencies
    validate_image_exists

    setup_workdir
    export_rootfs
    create_disk_image
    format_partitions
    extract_rootfs
    convert_to_qcow2
    write_manifest
    fix_ownership
    print_summary
}

main "$@"
