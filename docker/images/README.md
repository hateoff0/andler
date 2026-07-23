# docker/images — base image for Android/Linux instances

Building what becomes the **backing file** for qcow2 overlays used by
Android and Linux instances of ANDLER (`services/andler-disk`). This is not the
same as `docker/dev/` — that builds and tests ANDLER itself
(daemon/CLI); here we build the guest system that later
*runs inside* an instance under QEMU.

## Architecture: one rootfs recipe, variants by Android version and package set

An Android instance is Linux + Waydroid. The mode (Android/Linux) inside
the instance is switched via overlay config, not by rebuilding the image (see
next section) — but the **Android version** (11 or 13) and **package set**
(VANILLA/GAPPS) are fixed at base image build time, because
`system.img`/`vendor.img` are different binary artifacts that physically
live on disk. These are two independent choice axes, and `build.sh` accepts both:

| Axis | Values | `--build-arg` | Default |
|---|---|---|---|
| Android version | `11` (LineageOS 18.1), `13` (LineageOS 20.0) | `ANDROID_MAJOR` | `13` |
| Package set | `VANILLA` (no Google services), `GAPPS` (with Google Play) | `ANDROID_VARIANT` | `VANILLA` |

```bash
docker/images/build.sh                # Android 13, VANILLA
docker/images/build.sh 11              # Android 11, VANILLA
docker/images/build.sh 13 GAPPS        # Android 13, GAPPS
docker/images/build.sh 11 GAPPS        # Android 11, GAPPS
```

One `base/Dockerfile`, four possible builds via
`--build-arg` — not four separate Dockerfiles: everything except
`system.img`/`vendor.img` (kernel, Mesa/venus, gamescope, systemd units)
is identical across variants.

**Android 11 is no longer updated upstream** — the latest `lineage-18.1`
build on SourceForge is dated 2025-06-28; going forward Waydroid only ships
`lineage-20.0` (Android 13, roughly monthly). `ANDROID_MAJOR=11`
still builds correctly (`fetch-waydroid-images.py` fetches
the latest file matching lineage-18.1 — that is the frozen release), just be aware that the image
won't get fresher on its own unless upstream resumes releases.

**GAPPS requires per-device Google certification**, not per-image —
`ro.serialno`/Android ID and similar identifiers in Waydroid
are bound to a specific installation at first boot, and if many
Android instances are spawned from the same GAPPS backing file
without randomizing those identifiers at the overlay level, they risk
receiving the same "identity" for Google Play — which can lead to
blocks/detection as the same cloned device. Resolving this
(identifier randomization at instance creation) is the job of `andlerd`,
not this builder; this is only noted as a known concern so it isn't
forgotten at integration time.

### Why `waydroid init` is not run during `docker build`

`waydroid init` checks for a real kernel binder driver and
fails without one ("Failed to load binder driver") — and inside a normal
`docker build` there is neither `/dev/binder` nor a real kernel, only filesystem
layers. Therefore:

1. **At build time** (`base/Dockerfile`) only the actual `system.img`/`vendor.img` files are
   downloaded — from the official Waydroid SourceForge project,
   via `base/rootfs/usr/local/lib/andler/fetch-waydroid-images.py`
   (parses the RSS feed of the release directory, picks the latest file for
   the desired version — no hardcoded build date in the filename). They are placed
   into `/etc/waydroid-extra/images/` — the documented auto-discovery path
   for preinstalled images.
2. **At first real instance boot** (under a real QEMU kernel
   with binder) — a one-shot `waydroid-init.service` calls
   `waydroid init -f`, which discovers the already-placed files and
   does not reach out to the network for OTA. `ConditionPathExists=!/var/lib/waydroid/waydroid.cfg`
   makes this idempotent — on repeated boots of the same instance it is
   skipped.

Alongside `system.img`/`vendor.img`, `andler-fetch-manifest.json` is placed —
exact filenames (with release date in the name), URLs, and variant taken at
build time. This same file is included in `<qcow2>.manifest.json` written by
`build-disk.sh` — so you can see how fresh the Android version inside a given
built disk is without manually parsing the filename.

The ARM→x86 translator (libhoudini/libndk) is intentionally not
installed — both in terms of scope and delivery method this is a separate
task of `andlerd` itself (installing/switching the translator on demand
inside a running instance), not part of the base image.

### Integrity of `system.img`/`vendor.img`

`fetch-waydroid-images.py` now verifies MD5 of the downloaded `.zip` against what
SourceForge publishes in the same RSS feed we already
parse for filenames (`<media:hash algo="md5">`, a Media RSS
extension — a real, confirmed capability of SourceForge with live data,
not fabricated; it requires no extra network requests). On
mismatch the build fails with a clear error BEFORE extraction; when
the hash is absent from the feed — it prints a warning and continues (not all
feed entries always contain it).

**Honest assessment of what this covers and what it doesn't:** it protects against corrupted downloads
(unreliable networks on multi-hundred-MB files) and file mix-ups. It does **NOT** protect
against compromise of SourceForge itself — the hash is published by the same
platform as the file, so a compromised source means a compromised
declared hash too. MD5 is also cryptographically weak against intentional substitution
(though here it fits — the goal isn't to withstand an attack, but to catch corruption).
Independent verification (cross-checking against what Waydroid publishes through
its own OTA protocol, not SourceForge) would be stronger, but reproducing
that protocol is a separate, unfinished task.

The same `<media:content filesize="...">` tag next to the hash gives the exact
file size — used to verify free space BEFORE downloading
(in both the temp directory and `/etc/waydroid-extra/images`, which are different
filesystems) instead of guessing with a constant; on insufficient space the build
fails immediately with a clear number in megabytes, rather than midway through downloading
a gigabyte-sized file with a vague write error.

## Architecture: one base image for both modes (Android/Linux)

An Android instance is Linux + Waydroid. Therefore the same base image
serves both "Android instance" and "Linux instance" — the difference is only in
which `systemd` target is set as `default.target` on the overlay disk
of a given instance:

| Mode | Target | What appears on screen |
|---|---|---|
| Android | `android.target` | `gamescope` + `waydroid show-full-ui`, nothing else |
| Linux | `multi-user.target` | bare tty1 console (root, autologin), no DE — the user installs whatever they want on top of the overlay |

Mode switching is the responsibility of the instance overlay config, not of rebuilding
the base image (the base image itself doesn't change; only the symlink
`/etc/systemd/system/default.target` on the instance's overlay disk changes).

## Contents

```
docker/images/
├── base/
│   ├── Dockerfile              # rootfs build (Arch + linux-cachyos + waydroid + gamescope)
│   └── rootfs/                 # file overlay, copied on top of archlinux:base
│       ├── etc/systemd/system/android.target
│       ├── etc/systemd/system/gamescope-waydroid.service
│       ├── etc/systemd/system/waydroid-init.service
│       ├── etc/systemd/system/waydroid-container.service.d/andler-init.conf
│       ├── etc/systemd/system/getty@tty1.service.d/autologin.conf
│       ├── etc/systemd/network/20-virtio-wired.network
│       ├── etc/pacman.d/hooks/95-andler-uki.hook
│       ├── etc/mkinitcpio.conf
│       ├── etc/kernel/cmdline
│       ├── etc/fstab
│       ├── usr/local/bin/andler-build-uki
│       └── usr/local/lib/andler/fetch-waydroid-images.py
├── build.sh                    # docker build (desired Android version) + build-disk.sh, single command
└── build-disk.sh                # rootfs → partitioned bootable qcow2 (root, loop devices)
```

## Why two stages (`docker build`, then a separate `build-disk.sh`)

`docker build` produces a container rootfs — a set of files, not a disk. A disk
bootable via UEFI (`FirmwareConfig`/OVMF, already implemented in
`andler-core`/`andler-qemu`) requires real GPT partitioning, an ESP partition,
and so on — this involves partitioning and loop devices that an ordinary
unprivileged `docker build` does not have. Therefore:

1. `base/Dockerfile` — portable, cacheable, reproducible part
   (what gets installed, what gets configured). Can be run in CI without `--privileged`.
2. `build-disk.sh` — short, maximally mechanical script
   (partitioning/mkfs/extraction/conversion), requiring root. Intentionally
   thin: all substantive logic lives in the Dockerfile.

## How it boots via UEFI without a bootloader

Instead of `systemd-boot`/GRUB — a single **Unified Kernel Image** (kernel +
initramfs + cmdline + os-release in one EFI binary), built by
`ukify` and placed on the UEFI fallback path `/efi/EFI/BOOT/BOOTX64.EFI`.
The firmware (OVMF) finds and loads it on its own, without NVRAM entries/`efibootmgr`
— convenient because each instance's `OVMF_VARS.fd` template starts with
empty NVRAM, and per-instance boot entry pre-population is currently not supported
by anything in `andler-firmware`.

When the kernel is updated **inside a running instance** (`pacman -Syu`)
the UKI is automatically rebuilt — `95-andler-uki.hook` invokes
`andler-build-uki` after every `linux-cachyos` install/update.
Detailed rationale for both decisions is in the comments of
`base/rootfs/usr/local/bin/andler-build-uki`.

## Host requirements

`docker build` (first stage) requires only Docker itself (uses
`docker buildx build`, with automatic fallback to legacy `docker build`
if the buildx plugin is not installed). `build-disk.sh` (second stage, `sudo`)
additionally requires: `sgdisk` (package `gdisk`/`gptfdisk`), `losetup`/`mkfs.ext4`
(usually already present — `util-linux`/`e2fsprogs`), `mkfs.vfat` (`dosfstools`),
`qemu-img` (`qemu-utils`/`qemu-img`), `python3`. The script will suggest
the exact package name for your distribution if anything is missing.

`build.sh` asks for the sudo password once, at the very beginning (before the long
build), and keeps the sudo session alive in the background — no second interruption
mid-work.

`docker/images/build.sh --clean` removes the intermediate docker image
(`andler-base-rootfs:...`) after `build-disk.sh` has successfully extracted
its contents into qcow2 — useful for one-off/final runs
when the image is no longer needed. By default the image **persists** — during active
`Dockerfile` edits this saves time on layer cache across rebuilds.

Separately from this — over several `Dockerfile` edit iterations, buildx cache
accumulates old, no-longer-needed layer variants (they don't appear in
`docker images` but take up space). Clean with: `docker buildx prune`
(or `docker buildx prune -af` for a full cleanup, including the cache of other
images, if you don't need it).

`build-disk.sh` refuses to overwrite an existing file at
`<output.qcow2>` (the script runs as root — silent overwriting of
an arbitrary path would be unsafe) — add `--force` if
rebuilding the same file intentionally. `build.sh` passes `--force`
automatically, since it computes a safe path from the git revision.

The final `.qcow2`/`.manifest.json` at the end of `build-disk.sh` are explicitly
transferred back to the invoking user's ownership (`chown` using
`$SUDO_UID`/`$SUDO_GID`), rather than remaining `root:root` — otherwise a normal
user couldn't even delete files in their own `$HOME` without
`sudo`. **If you already have such root-owned files from runs before this
fix** — clean up once manually:
`sudo chown -R $USER:$USER ~/.andler/cache`.

## Quick start

```bash
# Everything at once (buildx build + build-disk.sh; sudo password asked once,
# right at the beginning, before the long build — no second interruption mid-work):
docker/images/build.sh                  # Android 13, VANILLA
docker/images/build.sh 11 GAPPS         # Android 11, GAPPS

# Or manually, step by step:
docker buildx build --load \
    -f docker/images/base/Dockerfile \
    --build-arg ANDROID_MAJOR=13 --build-arg ANDROID_VARIANT=VANILLA \
    -t andler-base-rootfs:android13-vanilla docker/images/base
sudo docker/images/build-disk.sh andler-base-rootfs:android13-vanilla \
    ~/.andler/cache/base-images/linux-waydroid-android13-vanilla-dev.qcow2 16G
```

The result is a `.qcow2` file plus a `.manifest.json` next to it (size,
sha256, build date, git revision, android_major/android_variant labels,
partitioning — `build-disk.sh` also prints the file size to the console on
completion). It is then used as the `backing_file` in the existing overlay
mechanism of `andler-disk`.

## Base image discovery

`core/andler-core/src/base_image.rs` scans `base_images_dir()`
(`~/.andler/cache/base-images/`, or `$ANDLER_HOME/cache/base-images/` if
`ANDLER_HOME` is set) for `*.manifest.json` files and picks the freshest one
(by `built_at`) matching a requested `AndroidProfile`'s Android version and
GApps/vanilla variant. `daemon/src/service.rs::create_android_instance`
uses this automatically whenever the client leaves `base_image_path` empty —
an explicit `--base-image-path` (CLI) or `base_image_path` (gRPC) still
overrides it, e.g. for a locally-built image not yet moved into the cache
directory. If no image is built yet, the resulting error names the exact
`docker/images/build.sh` invocation that will produce one.
