# docker/images — base image for Android/Linux instances

Building what becomes the **backing file** for qcow2 overlays used by
Android and Linux instances of ANDLER (`services/andler-disk`). This is not the
same as `docker/e2e/` — that builds and tests ANDLER itself
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
`system.img`/`vendor.img` (kernel, Mesa/venus, weston, systemd units)
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
a gigabyte-sized file with a vague write error. After download, the *unpacked* size
of the zips (sum of uncompressed member sizes) is checked against the extraction
directory before unpacking, so a too-small filesystem is caught before extraction
instead of halfway through it.

### Why the fetch step is fast(er) and never looks hung

The two zips (~1.1 GiB for Android 13 VANILLA, ~1.4 GiB for GAPPS) are downloaded
**in parallel**, with a progress line every 10 seconds (`MiB / MiB (%), speed, ETA`)
so a long download on a slow mirror shows liveness instead of a frozen build.
Interrupted downloads **resume over HTTP Range** (SourceForge mirrors serve 206
responses): a build killed mid-download continues from the last byte on the next
attempt, and a connection that stalls (no data for 2 minutes) reconnects from the
current position automatically. Each file has a 90-minute deadline; when it is hit,
the failure message says the resume will continue on the next run.

The fetch step runs with a **BuildKit cache mount**
(`RUN --mount=type=cache,target=/var/cache/andler-fetch` in `base/Dockerfile`);
verified zips live there between builds, so any rebuild that invalidates the fetch
layer (new rootfs overlay, different Android version) re-checks MD5 and **skips the
download entirely** instead of pulling another ~1 GiB from SourceForge. The cache
keeps only the two zips of the most recent successful build (older ones are removed).
Clearing it forces a fresh download: `docker builder prune`. This is also why
`build.sh` now requires `docker buildx` — the legacy `docker build` builder cannot
express cache mounts.

The fetch logic is covered by `docker/images/tests/test-fetch.sh`, a fixture suite
that runs the real script against a local Range-capable HTTP server and exercises
fresh download, cache reuse (asserting zero re-download requests), byte-level resume,
and the MD5-mismatch fatal path.

### Build-time package sources and their failure modes

The rootfs installs from three pacman repositories: official Arch
(`core`/`extra`), CachyOS (`linux-cachyos` plus the v3/v4 package sets) and
Chaotic-AUR.

Chaotic-AUR is the only one whose bootstrap hardcodes downloadable URLs, and it
is the one that has taken builds down: `cdn-mirror.chaotic.cx` answers 503 while
its regional mirrors serve the same files. The Dockerfile now takes the keyring
and mirrorlist from the first mirror that answers (`geo → cdn → us-ca → sg →
de`), and if none does, the build continues *without* the repository — every
package this image installs resolves from official + CachyOS (checked
2026-09-13: `waydroid` is in `extra` at 1.6.3-1, and the full install list of
~20 packages resolves without Chaotic-AUR). A package that later needs
Chaotic-AUR still fails loudly at the install step with `target not found`, so
the fallback cannot hide a broken dependency list.

Removing the repository outright would drop a flaky external dependency, but it
also changes which `waydroid` build ends up in the image (Arch's packaged
version instead of the AUR build), and that needs a full build plus a boot check
on a KVM host — a deliberate follow-up, not a drive-by change.

## Architecture: one base image for both modes (Android/Linux)

An Android instance is Linux + Waydroid. Therefore the same base image
serves both "Android instance" and "Linux instance" — the difference is only in
which `systemd` target is set as `default.target` on the overlay disk
of a given instance:

| Mode | Target | What appears on screen |
|---|---|---|
| Android | `android.target` | `weston` (kiosk shell) + `waydroid show-full-ui`, nothing else |
| Linux | `multi-user.target` | bare tty1 console (root, autologin), no DE — the user installs whatever they want on top of the overlay |

Mode switching is the responsibility of the instance overlay config, not of rebuilding
the base image (the base image itself doesn't change; only the symlink
`/etc/systemd/system/default.target` on the instance's overlay disk changes).

### Why the Android session runs on Weston, not gamescope

The Android session compositor must present a frame on **any** host GPU, and the
GPU paths in front of it differ in a way that matters:

- **GL (virgl)** — the guest renders through `virtio-gpu`'s GL context, the host
  rasterizes it (on the physical GPU), and the resulting buffer scans out through
  KMS. This never depends on host DRM modifier support. Proven end-to-end by the
  Linux instance: its KDE (KWin) session composites exactly this way and works on
  NVIDIA, AMD, and Intel hosts alike.
- **Vulkan (venus)** — the guest gets a venus Vulkan device backed by the host's
  physical GPU (and a second one backed by lavapipe). Scan-out of venus buffers
  requires DRM format modifiers, and not every host driver provides them:
  NVIDIA's driver exposes none for scanout formats, so `DMA-BUF import` into KMS
  fails with "format/modifier not supported for scan-out".

The previous session used **gamescope**, which *only* presents through Vulkan.
On NVIDIA-backed venus devices its every frame import failed and it crash-looped
with a black screen (plus a burst of `vkr` host-side errors, all consequences of
the same import failures). That was structural to the stack, not a virglrenderer
bug on the host.

The session now runs **Weston** with its DRM backend and the kiosk shell: it
composites through GL (default `renderer=auto`, GL with a pixman fallback), never
allocates Vulkan buffers for scanout, and presents the waydroid window fullscreen
on any host GPU. See `waydroid-compositor.service` and
`usr/local/bin/andler-waydroid-compositor` for the exact wiring.

This does **not** disable venus: QEMU still creates `virtio-gpu-gl` with
`venus=true`, so the guest keeps a working Vulkan device for Android apps that
want it (fully functional on hosts whose venus device exposes modifiers —
AMD/Intel; rendering-only on NVIDIA). The split is: **venus = Vulkan for guest
apps, virgl = display/compositing** — each works wherever the host allows it.

Weston also means no Vulkan dependency at all in the session path; a hypothetical
gamecope-with-lavapipe fallback would have traded the black screen for CPU
rendering and an uncertain modifier story, with nothing gained.

Output resolution: `andler-waydroid-compositor` appends
`[output] name=Virtual-1 mode=…` to the base weston.ini at startup
(`Virtual-1` is the virtio-gpu connector's kernel name, as logged by weston).
The default is **1920x1080** — it exists in the virtio-gpu EDID mode list
(verified in a boot log: `Output Virtual-1 … video modes: … 1920x1080@60.0`),
so weston accepts it and the SDL window follows. Per-instance override: the daemon passes the instance config's
`display.resolution` into the VM as QEMU fw_cfg
(`-fw_cfg name=opt/andler/display-resolution,string=WxH`), and a guest
oneshot (`andler-display-resolution.service`) writes it to
`/etc/andler/display.conf` before the compositor starts — so the instance's
configured resolution is what weston applies (any WxH that exists in the EDID
mode list; weston refuses modes absent from it).

Changing the resolution on a **running** VM is a host-side command — no guest
login needed:

```
andler config set <id> display.resolution 1920x1200
```

The daemon updates `display.resolution` in the instance config (takes effect
on the next boot via fw_cfg) and, when the VM is running, pushes the new
value into the guest over the QEMU guest agent (`qemu-guest-agent` runs in
the image; the virtio-serial port is wired in `cmdline.rs`): Android restarts
the waydroid compositor session (2-3 s, the VM keeps running), Linux runs
`andler-apply-resolution` which switches the active session's output
(kscreen-doctor/gnome-randr/wlr-randr on Wayland, xrandr on X11) — Plasma
then keeps the mode in its kscreen config. On a stopped instance the command
only updates the config. `andler-set-resolution WxH` inside the guest still
works for quick experiments, but lasts only until the next boot replay of
fw_cfg.

The Linux VM (KDE Plasma) reads the same EDID, so its available modes include

The Linux VM (KDE Plasma) reads the same EDID, so its available modes include
1920x1080 as well. The guest applies the instance resolution automatically at
session start: `andler-apply-resolution` (XDG autostart entry
`/etc/xdg/autostart/andler-apply-resolution.desktop`) reads
`/etc/andler/display.conf` and switches the output via `kscreen-doctor
output.Virtual-1 mode.WxH` (Wayland) or `xrandr --output Virtual-1 --mode WxH`
(X11); Plasma then keeps the chosen mode in its kscreen config for later
boots. The KDE VM just needs `andler-apply-resolution` plus the .desktop file
copied into its image. Mode changes on the live Android VM: `andler-set-resolution WxH`
inside the guest (writes display.conf, restarts the compositor session — the
VM keeps running; the change lasts until the next boot replays fw_cfg from the
instance config).

### Session startup ordering (why nothing restarts twice)

Both pipewire units use `PAMName=login`, whose PAM stack includes pam_nologin:
starting them before `systemd-user-sessions.service` has removed `/run/nologin`
fails with "System is booting up. Unprivileged users are not permitted to log
in yet." — then `Restart=on-failure` bounces them, and since
`waydroid-compositor.service` has `Requires=andler-pipewire-pulse.service`, the
whole weston+waydroid session restarts mid-start (two westons, two container
starts — and the composer@2.1-se abort inside the first, interrupted container
boot). The units order themselves `After=systemd-user-sessions.service`, so the
first start succeeds and nothing restarts.

### Known cosmetic noise

- `unknown libinput event 404` from weston: libinput ≥ 1.26 sends the new
  `LIBINPUT_EVENT_POINTER_SCROLL_WHEEL` (404) *in addition to* the legacy
  `LIBINPUT_EVENT_POINTER_AXIS` (403) that weston 15 consumes — scrolling works,
  weston just logs the duplicate it doesn't handle. No config knob; harmless.

### Guest DNS (why `/etc/resolv.conf` is a stub symlink)

`docker build` injects a resolv.conf during RUN steps but never bakes one into
the image layers — so a disk extracted from the image has no `/etc/resolv.conf`
at all. The guest itself doesn't notice: systemd-resolved resolves through the
per-link DHCP DNS (slirp's `10.0.2.3`). The **waydroid container's** DNS does
notice: waydroid's dnsmasq on `waydroid0` reads `/etc/resolv.conf` for its
upstream and, with no file, logs `no servers found in /etc/resolv.conf, will
retry` forever — the container gets an IP via DHCP but every name lookup fails
(`ERR_NAME_NOT_RESOLVED` in Android apps). The image therefore ships a
systemd-tmpfiles rule (`rootfs/etc/tmpfiles.d/andler-resolv.conf`) that
creates `/etc/resolv.conf` at boot as a symlink to systemd-resolved's stub
(`/run/systemd/resolve/stub-resolv.conf`), giving the container the chain
dnsmasq → stub `127.0.0.53` → resolved → `10.0.2.3` → slirp → host. Two gotchas
make this non-trivial, both handled by the rule: a Dockerfile `RUN ln` cannot
bake the symlink (docker build bind-mounts its own resolv.conf over the path,
so `ln` fails with EBUSY), and `docker export` (used by `build-disk.sh`)
injects an **empty** regular `/etc/resolv.conf` into the rootfs tar — which
shadows everything, since a plain `L` rule silently skips existing files and
Arch's own `L!` rule (`/usr/lib/tmpfiles.d/systemd-resolve.conf`) is dropped
as a duplicate. The rule therefore uses `L+`, which unconditionally replaces
whatever sits at `/etc/resolv.conf` with the stub symlink.

## Contents

```
docker/images/
├── base/
│   ├── Dockerfile              # rootfs build (Arch + linux-cachyos + waydroid + weston)
│   └── rootfs/                 # file overlay, copied on top of archlinux:base
│       ├── etc/andler/weston.ini
│       ├── etc/fstab
│       ├── etc/kernel/cmdline
│       ├── etc/mkinitcpio.conf
│       ├── etc/pacman.d/hooks/95-andler-uki.hook
│       ├── etc/systemd/journald.conf.d/andler-console-forward.conf
│       ├── etc/systemd/network/20-virtio-wired.network
│       ├── etc/systemd/system/android.target
│       ├── etc/systemd/system/andler-display-resolution.service
│       ├── etc/systemd/system/andler-pipewire.service          # user session, PAMName=login
│       ├── etc/systemd/system/andler-pipewire-pulse.service
│       ├── etc/systemd/system/qemu-guest-agent.service         # custom: not tied to a virtio-ports device unit
│       ├── etc/systemd/system/waydroid-compositor.service      # dbus-run-session + Weston DRM on tty1
│       ├── etc/systemd/system/waydroid-init.service
│       ├── etc/systemd/system/waydroid-container.service.d/andler-init.conf
│       ├── etc/systemd/system/getty@tty1.service.d/autologin.conf
│       ├── etc/systemd/system/multi-user.target.wants/         # symlinks: display-resolution + qemu-guest-agent
│       ├── etc/tmpfiles.d/andler-resolv.conf                   # L+ /etc/resolv.conf → stub (waydroid dnsmasq upstream)
│       ├── etc/vconsole.conf
│       ├── etc/xdg/autostart/andler-apply-resolution.desktop
│       ├── usr/local/bin/andler-apply-resolution
│       ├── usr/local/bin/andler-build-uki
│       ├── usr/local/bin/andler-display-resolution
│       ├── usr/local/bin/andler-set-resolution
│       ├── usr/local/bin/andler-waydroid-compositor
│       ├── usr/local/bin/andler-waydroid-session
│       └── usr/local/lib/andler/fetch-waydroid-images.py
├── build.sh                    # docker build (desired Android version) + build-disk.sh, single command
├── build-disk.sh               # rootfs → partitioned bootable qcow2 (root, loop devices)
└── tests/
    ├── fixture_server.py       # local Range-capable HTTP server (fetch tests)
    └── test-fetch.sh           # fetch-waydroid-images.py: download/cache/resume/md5 paths
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
    ~/.andler/cache/base-images/android13-vanilla/linux-waydroid-android13-vanilla-dev.qcow2 16G
```

`build.sh` writes into a per-version-variant subdirectory of the cache by
default (`~/.andler/cache/base-images/android13-vanilla/`); the manual
`build-disk.sh` invocation above takes any path ending in `.qcow2`.

The first build downloads the Waydroid system/vendor zips (≈1.1–1.4 GiB):
progress lines every 10s, parallel system+vendor, resumable over HTTP Range
(a killed build continues on the next run). Verified zips are cached in a
BuildKit cache mount, so rebuilding after a rootfs change skips the download
(`docker builder prune` forces a fresh one). Step 1 requires `docker buildx`.

The result is a `.qcow2` file plus a `.manifest.json` next to it (size,
sha256, build date, git revision, android_major/android_variant labels,
partitioning — `build-disk.sh` also prints the file size to the console on
completion). It is then used as the `backing_file` in the existing overlay
mechanism of `andler-disk`.

## Downloading a published image instead of building one

The same builds are published as GitHub releases by
`.github/workflows/build-base-image.yml` (tag
`base-image-android<major>-<variant>-<timestamp>`; assets: one
`<stem>.manifest.json` plus `<stem>.qcow2.zst.NN.part` files, because a release
asset is capped at 2 GiB). The runtime side of that contract lives in
`services/andler-disk/src/base_image_download.rs`:

```bash
andler image list                       # what the pipeline has published
andler image download --android-version 13 --variant vanilla
```

A downloaded image lands in the same cache this document describes
(`~/.andler/cache/base-images/android<major>-<variant>/<stem>.qcow2` plus the
published manifest), so `base_image::resolve`, `andler cache list|clean`, and
`andler create --base-image-path` treat it exactly like a locally built one. The
daemon reads the catalog from `ANDLERD_IMAGE_REPO` (default `hateoff0/andler`)
and `ANDLERD_IMAGE_API_BASE` (default `https://api.github.com`).

## Base image discovery

`core/andler-core/src/base_image.rs` scans `base_images_dir()`
(`~/.andler/cache/base-images/`, or `$ANDLER_HOME/cache/base-images/` if
`ANDLER_HOME` is set) for `*.manifest.json` files — both directly in the
cache root (legacy layout) and in one-level subdirectories such as
`android13-vanilla/` (the `build.sh` default) — and picks the freshest one
(by `built_at`) matching a requested `AndroidProfile`'s Android version and
GApps/vanilla variant. `apps/daemon/src/service.rs::create_android_instance`
uses this automatically whenever the client leaves `base_image_path` empty —
an explicit `--base-image-path` (CLI) or `base_image_path` (gRPC) still
overrides it, e.g. for a locally-built image not yet moved into the cache
directory. If no image is built yet, the resulting error names the exact
`docker/images/build.sh` invocation that will produce one.
