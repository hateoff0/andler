# Changelog

All notable changes to ANDLER will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]


### Added

#### CLI

- **`andler doctor` command**: Checks local environment health — KVM, QEMU, OVMF, nbd module, passwordless sudo, daemon reachability, base images. `--fix` flag offers to write missing sudoers rules via `visudo`.
- **`andler attach` / `andler detach`**: Hot-plug extra disks (`attach disk --path <p> --size <s>` / `detach disk <p>`) and network devices (`attach net [--mode nat|bridge|isolated] [--bridge <if>] [--model <m>] [--nat-backend slirp|passt]` / `detach net <index>`) into a running/paused instance. Attached devices are persisted in `instance.toml` (`extra_disks`/`extra_networks`) and re-created automatically on the next start. Detaching a disk never deletes the image file.

#### Backend (`andler-qemu`)

- **Serial console logging**: `-serial file:console.log` captures QEMU serial output to a file next to the instance disk for debugging.
- **`window-close=off`**: SDL and GTK display windows no longer close the VM when the window is closed — prevents accidental shutdown.
- **Device hot-plug**: `blockdev-add`/`device_add` (virtio-blk-pci, virtio-net-pci) and `netdev-add` (user/tap/passt) over QMP for extra disks and networks, plus async-aware `device_del` (retries `blockdev-del`/`netdev-del` only while QEMU reports the device in use). Extra devices are also wired into the boot command line (`drive-extraN`/`net-extraN`, bridge taps embed the first 8 hex chars of the instance id), so attached devices reappear after a restart. Host tap/veth lifecycle for bridge mode is set up before spawn and torn down on stop, with rollback on failure.

#### Daemon

- **Disk-only snapshots**: `snapshot create`/`delete` now use synchronous `blockdev-snapshot-internal-sync`/`-delete-internal-sync` (qcow2 internal snapshots) instead of the vmstate job API — they work on any GPU/audio/CPU configuration, including the defaults (`virtio-sound`, Venus, `invtsc`), which QEMU's migration machinery refuses to serialize. `snapshot restore` is an offline `qemu-img snapshot -a` that requires a stopped instance (`InstanceMustBeStopped`); the guest boots from the snapshot on next start (RAM is not restored).
- **Hotplug ops**: `attach_disk`/`detach_disk`/`attach_network`/`detach_network` daemon operations, gated on `Running`/`Paused` (`HotplugRequiresRunningInstance` otherwise). New extra disks are created as qcow2 (size 0 rejected, existing images report their actual virtual size), config is persisted after the backend confirms, and a failed backend op rolls back a just-created image file. Duplicate paths (including the primary disk) → `DiskAlreadyAttached`; detaching an unattached device → `DiskNotAttached`/`NetworkNotAttached`. `validate_instance_files` now also checks extra disk paths at boot; `remove --purge` deletes extra disk files that live inside the instance directory and leaves out-of-dir user data untouched.
- **Pre-start file validation**: `validate_instance_files()` checks disk and firmware paths exist before spawning QEMU, catching deleted/moved instance directories early instead of letting QEMU fork and fail silently.
- **Structured lifecycle tracing**: `info`/`error` tracing for all instance lifecycle operations (create, start, stop, pause, resume, remove) with `instance_id` and error details.
- **`InstanceAlreadyStopped` error**: Clear error message when stopping an already-stopped instance, instead of a generic error.
- **`schema_version` in `instance.toml`**: config files now carry the instance config schema version; files that predate schema versioning read back as v1. `andler-core` provides `migrate_schema()`, which applies pending migrations on load and refuses configs newer than the daemon (future versions fail loudly instead of being misread).
- **Per-instance supervisor**: every registered instance runs a dedicated tokio task that owns its FSM state, backend handle and config (all reads/writes funnel through `watch` snapshots and an acknowledged command channel). `DaemonError` is classified through one exhaustive `ErrorKind` match; an instance-level event bus (`subscribe_events`) carries lifecycle transitions and failures; supervisor task panics are logged with the instance id instead of silently closing its command channel.
- **Instant QEMU death detection**: a killed/crashed QEMU process is now detected via pidfd death notification (no 30s health-check poll) — the instance lands in `Error { QEMU process exited unexpectedly }` within milliseconds, and `Handler logs` etc. keep working.
- **Daemon restart reconnect**: after `andlerd` is SIGKILLed/crashes, a restarted daemon adopts instances that were `Running`/`Paused` by re-attaching to the surviving QEMU process (identity verified through QMP and the `process=<name>` cmdline marker) instead of marking them `Error`. Instances that were mid-operation on restart are marked `Error` with an “unknown result” message pointing at `andler status` / `andler snapshot list`.

### Changed

#### Structure

- **`apps/` group**: `cli/` and `daemon/` moved under `apps/` — every crate now lives in a role group (`core/`, `backends/`, `services/`, `apps/`) at `<group>/<crate>/`. Package names and binary names (`andler`, `andlerd`) unchanged.
- **`docker/dev` → `docker/e2e`**: the containerized test harness moved to `docker/e2e/` (`Dockerfile`, `compose.yaml`, `e2e.sh`). The single E2E smoke script was replaced by a modular suite (`docker/e2e/tests/NN_*.sh`) covering all CLI commands, with a shared assertion library; builds are incremental (BuildKit cache mounts, no `--no-cache`); the e2e image is a slim Debian runtime with QEMU/OVMF; deep guest tests (offline install/remove/list, boot-mode switching) run against a baked Debian rootfs via qemu-nbd and SKIP when the `nbd` module is unavailable.

#### Services (`andler-disk`)

- **Single privileged entry point (`andler-helper`)**: privileged operations (nbd connect/disconnect, `modprobe nbd max_part=8`, partition/bind/tmpfs mounts and lazy unmounts, chrooted package manager runs, guest-filesystem writes and file ops) are no longer ten separate `NOPASSWD` sudoers rules for ten system tools — they are subcommands of one root-owned helper binary `/usr/local/sbin/andler-helper` (new crate `apps/helper`, std-only + libc, zero other deps) authorized by a single rule `youruser ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper`. The helper validates every argument itself (paths must resolve inside the NBD-mounted guest partition — the `file cp-a` source is a read-only host path from the translator cache, and the `guest-write` target is guest-relative — devices must be free `/dev/nbd*`, mount points must be owned by the invoking user, chroot commands come from an `apt-get|apt|dnf|pacman|ln` allowlist) and never shells out. `andler doctor`/`--fix` now checks and installs the helper (root:root 0755) and migrates legacy per-binary rules away; `uninstall.sh --purge` removes the binary too. Keeps the daemon unprivileged while shrinking the sudoers surface from ten rules to one. Guest resolv.conf writes no longer round-trip through a host temp file (`guest-write` reads stdin and writes directly inside the chroot).
- **Guest package detection checks multiple binary paths**: `GuestPackage` now carries `binary_checks: &[&str]` instead of a single `binary_check`; `guest list` reports a package installed when any candidate exists (`/usr/bin/qemu-ga` *or* `/usr/sbin/qemu-ga`, likewise `spice-vdagentd`). Distros differ in where they install the same tool — Debian/Ubuntu put `qemu-ga` in `/usr/sbin`, Arch in `/usr/bin` — so a Debian guest previously showed `qemu-guest-agent` as `not installed` right after a successful install.

#### CLI

- **GApps prompt moved earlier in wizard**: Base image variant selection (VANILLA vs GApps) now happens before base image choice, so the wizard uses it to filter available images. Forwarded through quick-mode.
- **Docker-style instance IDs**: Instance IDs are 64 lowercase hex chars (formatted like docker/SHA IDs) instead of UUIDv4. Human output (`create`, `clone`, `start`/`stop`/`pause`/`resume`, `remove`, `list`, `config`) shows the 12-char short ID; the full ID is available via `list --full-id`, `list --json`, and the `~/.andler/instances/<id>/` directory name. Instance references resolve by any unique hex prefix (existing behavior, now hex-only). Old UUID-named instance data is not migrated — remove and recreate.

#### Guest images (`docker/images`)

- **Waydroid image fetch is resumable, parallel and visible**: `fetch-waydroid-images.py` now downloads system+vendor zips in parallel with periodic progress lines (MiB, %, speed, ETA) — a ~1.4 GiB download previously ran silently and looked like a hang. Interrupted downloads resume over HTTP Range (SourceForge mirrors serve 206), stalled connections (no data for 2 min) reconnect from the last byte, and MD5-verified zips are cached across builds via a BuildKit cache mount (`RUN --mount=type=cache` in `base/Dockerfile`), so a rebuild after any earlier layer change skips the re-download entirely; the cache keeps only the two current zips. Upstream files that fail MD5 after a fresh download abort the build with an actionable error; the unpacked size of the zips is now checked against the extraction directory before unpacking, instead of a compressed-size estimate. `build.sh` now requires `docker buildx` (the cache mount cannot be expressed by the legacy builder). Covered by a fixture-based test suite (`docker/images/tests/test-fetch.sh`, local HTTP server with Range support) exercising fresh download, cache reuse, resume, and the MD5-mismatch fatal path.

### Fixed

#### Store (`andler-store`)

- **Snapshot metadata destroyed on every state change**: `save_instance` used `INSERT OR REPLACE`, which SQLite implements as DELETE + INSERT; combined with the `snapshots.instance_id ... ON DELETE CASCADE` foreign key, every `persist_state` (start, stop, pause, resume) silently deleted all snapshot metadata for the instance. `snapshot list` for a stopped instance returned nothing, and the records were unrecoverable (the qcow2 snapshots themselves were never touched). `save_instance` now uses `ON CONFLICT DO UPDATE`, which updates in place and never fires the cascade. Found by the new E2E snapshot suite; regression-covered by `save_instance_does_not_cascade_delete_snapshots`.

#### Services (`andler-disk`)

- **ARM translator download could hang forever**: `translator_download` used `reqwest::get` with no timeouts — against a slow or silently-dropping connection (GitHub unreachable, filtered network) `andler guest install libndk|libhoudini` sat indefinitely with no output and no daemon log line. The download now has a 15 s connect timeout and a 5 min total timeout, logs each stage (download → md5 → extract), and its error points at `--translator-dir <path>` as the offline workaround. The CLI prints a progress line before starting. Regression-covered by two local tests (silent-server timeout, HTTP error status).
- **Offline `guest remove` was refused even after a successful install**: `is_agent_installed` ran `chroot <mount> <manager> dpkg -l <pkg>` with the query binary passed as an *argument* to the manager — `apt-get dpkg` is not a valid invocation ("unknown command"), so the package was always reported as not installed and removal was refused. The install-state query now uses the manager's own query command (`dpkg -l` for Apt, `rpm -q` for Dnf, `pacman -Qi` for Pacman) via `PackageManager::check_installed_command`; the online QGA probe checks every candidate binary path too.
- **Never-booted installs keep the base image's properties**: pre-boot translator installs (whose overlay-dir creation is described in the `Daemon` section below) start the `build.prop` merge from the base image's `/system/build.prop` when the upper file doesn't exist yet — an upper `build.prop` shadows the base file wholesale, so seeding from an empty set would have dropped the ROM's `ro.*` properties. `initial_props()` picks upper file → base file → empty set.
- **Translator downloads are actually installed**: the GitHub `<repo>-<commit>/prebuilts/` archive wrapper is now flattened at extraction, so the cache hit-detection matches and the payload lands under `bin/`, `etc/`, `lib/`, `lib64/` where the installer looks. The `lib/libndk*` wildcards now expand against the real archive contents — the previously silent drops (`etc/binfmt_misc`, `etc/cpuinfo.*`, `etc/ld.config.*`, both `bin/ndk_translation_program_runner_binfmt_misc*` runners) are installed and removed like the rest of the payload. A collision between an already-flattened payload dir and the incoming archive fails loudly instead of silently re-downloading forever, and already-flat payload dirs are left untouched.
- **`libhoudini` gets a working init script and working pins**: `houdini.rc` is replaced with the waydroid-helper reference script (byte-identical, mounts `binfmt_misc` and registers the four arm/arm64 handlers), and the downloads are re-pinned to the working waydroid-helper prebuilt commits (Android 11 `cf7f970f…`, Android 13 `debc3dc9…`). `bin/arm`/`bin/arm64` keep their place in the file list — the Android 11 archive ships the ARM linkers under them; the Android 13 archive lacks that tree and the expander skips the missing entries with a warning.
- **No stale translator props**: `build.prop` translator keys are now managed from a fixed key list (`MANAGED_PROP_KEYS`, incl. `ro.vendor.enable.native.bridge.exec[64]` and `ro.ndk_translation.version`): every switch (including to `None`) removes the old keys before writing the new ones, so `libndk` → `libhoudini` can no longer leak `ro.ndk_translation.version` or stale bridge props.
- **ABI list order matches the Google/WSA reference**: `ro.product.cpu.abilist` is now `x86_64,arm64-v8a,x86,armeabi-v7a,armeabi` (arm64 first), matching the order the translation layer expects.
- **`andler doctor covers the translator file tools`**: the offline switch stages and moves files on the mounted guest partition through `sudo -n mkdir/cp/mv/rm/chmod` (the guest files are root-owned), and doctor now checks those NOPASSWD rules alongside the nbd/chroot ones; `--fix` writes them with the rest.
- **`andler doctor --fix` no longer drops existing sudoers rules**: `/etc/sudoers.d/andler` is `0440 root:root` once written, and the old read-back path (`sudo -n cat`) was never NOPASSWD-authorized, so a later `--fix` saw an "empty" file and *overwrote* it — silently erasing the rules an earlier `--fix`/`install.sh` had added (e.g. `qemu-nbd` disappeared and offline guest operations started failing with "a password is required"). The file is now read back through the already-authorized `chroot` rule (`sudo -n chroot / cat`), new rules are merged with what's already there, and `--fix` refuses to touch a file it cannot read instead of overwriting it.

#### Daemon

- **Misleading "to change config" error on disk-touching guest ops**: `InstanceMustBeStopped` said "must be stopped … to change config", which was wrong for `guest install/remove libndk|libhoudini` and `guest boot-mode` (disk-staging operations, not config edits). The message is now operation-neutral: "must be stopped … to perform this operation; stop it first".
- **Paused instances got a "VM is running" hint when the guest agent can't respond**: `guest install/remove <pkg>` on a `Paused` VM now explains that the frozen guest can't answer and suggests resuming or stopping instead of claiming the VM is running.
- **ARM translator install failed on never-booted Android instances**: `waydroid init` creates `/var/lib/waydroid/overlay` only on the guest's first boot, so `guest install libndk|libhoudini` on a fresh instance failed with "Waydroid overlay directory not found in guest filesystem". The overlay upper dir is just a directory tree bind-mounted over `/system`, so the daemon now creates `overlay/system` itself when missing — translator installs work before the first boot. This also fixed the write path itself: andlerd runs unprivileged, so raw `std::fs` writes into the rw-mounted guest partition failed with EPERM on the guest's root-owned directories — all translator file mutations (mkdir/cp/mv/rm/chmod/build.prop) now go through `sudo -n`, the same privileged pattern `guest_tools.rs` and `boot_mode.rs` already use. Regression-covered by `detect_waydroid_system_dir_*` and `build_prop_content_*` tests.

### Added

#### Core (`andler-core`)

- **Unified `create` command**: Single `create` command with `--kind linux`/`--kind android` flag to select VM type. TOML mode auto-detects type from content.
- **Base image cache subdirectories**: `docker/images/build.sh` now writes images into `cache/base-images/android<version>-<variant>/` (one-level subdirectories); discovery (`base_image::list_matching`/`resolve`) scans both the new layout and the legacy flat root, so pre-existing images keep working without migration. New `base_image::list_all()` powers `andler doctor`'s base-image check. Covered by `resolve_finds_image_in_version_variant_subdirectory`, `resolve_prefers_freshest_across_root_and_subdirectory`, `list_all_includes_flat_and_subdirectory_images`, the gRPC round-trip auto-resolve test, and E2E suite `10_base_images.sh`.

#### CLI

- **Android overlay default raised to 128 GiB**: `--overlay-size-gib`, the TOML `overlay_size_gib` default, and the wizard's fixed default all changed from 20 GiB to 128 GiB — the overlay's virtual size, thin-provisioned qcow2, so it costs no disk up front; users can shrink via `--overlay-size-gib` or grow later.
- **Hotplugged devices in `InstanceConfig`**: `extra_disks: Vec<DiskConfig>` and `extra_networks: Vec<NetworkConfig>` (serde-defaulted for existing `instance.toml` files); new `HypervisorBackend` methods `attach_disk`/`detach_disk`/`attach_network`/`detach_network` (default `NotImplemented`).

#### RPC (`andler-rpc`)

- **`AttachDisk`/`DetachDisk`/`AttachNetwork`/`DetachNetwork`**: four new unary RPCs for live device hot-plug; `GetInstanceConfigResponse`/`UpdateInstanceConfigRequest` gained `extra_disks`/`extra_networks` repeated fields so attached devices round-trip through get-edit-put.
- **`--kind` flag**: `--kind linux` creates LinuxVm via CLI flags (`--iso-path`, `--disk-path`, `--ovmf-vars-template`). `--kind android` creates AndroidVm via CLI flags. Mutually exclusive with `--file`.
- **AndroidVm from TOML**: `InstanceFile` supports `android_version`, `base_image_path`, `overlay_size_gib`, `gapps`, `microg`, `libndk`, `instances_root`. Auto-detected: presence of `android_version` or `base_image_path` → AndroidVm; otherwise LinuxVm.
- **LinuxVm clone/export**: `CloneMode::Linked` and `CloneMode::FullStandalone` supported. `SharedBase` rejected with `SharedBaseNotSupportedForLinuxVm`.
- **Path utilities**: `runtime_dir()` (XDG_RUNTIME_DIR fallback), `current_uid()` (getuid FFI), `ensure_private_dir()` / `ensure_private_dir_sync()` (0700 permissions).
- **`ensure_qcow2_extension()`**: Auto-appends `.qcow2` extension to disk paths.
- **FSM: restart from `Stopped`/`Error`**: `Start` is now a valid transition from both (-> `Starting`), same path as a fresh `Created` instance. Found while implementing VM health checks that this wasn't previously possible at all — not just for crashed instances, `andler start` didn't work on a manually-stopped one either. `Daemon::start_instance` needed no changes (it was already generic over the source state). `is_terminal()` keeps its previous meaning ("this run has ended"), not "no outgoing transitions exist".

#### Backend (`andler-qemu`)

- **Disk-only internal snapshots (backend)**: `snapshot_save`/`snapshot_delete` QMP methods now use synchronous `blockdev-snapshot-internal-sync`/`-delete-internal-sync` (qcow2 internal snapshots, `device`+`name` wire schema — no job-id/vmstate/devices) instead of the vmstate job API. The old `snapshot-save`/`snapshot-load` path serializes through QEMU's migration machinery, which blocks on every default component (`virtio-sound`, virgl, `invtsc` CPU flag) — it was removed, not kept as a fallback. `wait_job_completion` remains as a generic QMP utility.
- **Resource metrics from `/proc`**: Real-time streaming of CPU%, RAM usage, disk I/O, and network I/O. No QMP required. 1-second polling interval.
- **Stale QMP socket cleanup on spawn**: removes a leftover socket file from a previous run before binding a new one — the per-instance QMP socket path is deterministic, so restarting the same instance (see FSM restart above) could otherwise fail to bind with "address already in use" even though nothing was actually listening there anymore.
- **GPU metrics (AMD)**: Sysfs-based GPU metrics — VRAM used/total and GPU load percentage from `/sys/class/drm/card*/device/`.
- **GPU metrics (NVIDIA)**: `nvidia-smi` CLI-based GPU metrics — VRAM used/total (MiB) and GPU load %. Automatic vendor detection with AMD→NVIDIA→Intel priority.
- **GPU metrics (Intel)**: i915 sysfs-based GPU metrics — GPU load % via `power/rc6_residency_ms` idle-time delta (documented i915 ABI; an earlier draft read a non-existent `busyiffies` path — never shipped). No VRAM metric — integrated Intel VRAM accounting isn't a stable sysfs ABI.
- **GPU vendor detection**: Automatic AMD → NVIDIA → Intel priority. First found vendor wins. Caches result to avoid repeated PATH lookups.
- **QMP connection recovery**: `pause`/`resume`/`status` no longer get stuck on a stale cached QMP connection after a transient disconnect — they clear it and reconnect once before failing. New `BackendError::ProcessNotRunning` distinguishes "QEMU process itself exited" (checked via `is_alive()`) from a recoverable QMP hiccup or a genuine command error (`CommandFailed`/`ParseError`, never retried).

#### Services

- **Disk error variants**: `ShrinkRequiresConfirmation` (requires `--shrink` flag), `CompactNotApplicable` (non-qcow2 disk), `InsufficientDiskSpace` (pre-checked before snapshot creation via `statvfs(2)`, using guest RAM size as a conservative upper bound for the snapshot's disk footprint — maps to `Status::resource_exhausted`).

#### Guest images (`docker/images`)

- **Android container DNS fixed**: the image now creates `/etc/resolv.conf` at boot (systemd-tmpfiles rule `rootfs/etc/tmpfiles.d/andler-resolv.conf`, `L+` overwrite) as a symlink to systemd-resolved's stub (`/run/systemd/resolve/stub-resolv.conf`). Root cause chain: `docker build` never bakes a resolv.conf into the image (the path is a Docker bind-mount during RUN steps), and `docker export` (used by `build-disk.sh`) injects an **empty** regular `/etc/resolv.conf` into the rootfs tar — a plain `L` rule silently skips the existing empty file and Arch's own `L!` rule is dropped as a duplicate path, so the waydroid container's dnsmasq had no upstream (`no servers found in /etc/resolv.conf`) and Android apps failed every name lookup with `ERR_NAME_NOT_RESOLVED` despite the guest itself being online. `L+` unconditionally replaces the empty file with the stub symlink (dnsmasq → `127.0.0.53` → resolved → `10.0.2.3` → slirp → host). See `docker/images/README.md`.

- **Resolution changes are a host-side command**: `andler config set <id> display.resolution WxH` updates the instance config (persisted, takes effect on next boot via fw_cfg) and, on a running VM, pushes the new value into the guest over the QEMU guest agent — new `virtio-serial` port + `qemu-guest-agent` package in the base image, and a new `HypervisorBackend::set_guest_display_resolution`. Guest commands (file writes, `guest-exec`) are sent straight to the agent chardev socket (`*.qga.sock`), not through QMP: QEMU ≥9 no longer registers `guest-*` commands on the QMP monitor. Android restarts the waydroid compositor session in place (VM keeps running); Linux applies the mode to the active session (kscreen-doctor/gnome-randr/wlr-randr on Wayland, xrandr on X11). The old guest-side `andler-set-resolution WxH` remains for experiments but is no longer the intended path.
- **Linux VM applies instance resolution at session start**: `andler-apply-resolution` (XDG autostart) switches the desktop output to the `display.resolution` from `/etc/andler/display.conf` via `kscreen-doctor` (Wayland) or `xrandr` (X11); Plasma remembers the mode in its kscreen config. `andler-set-resolution WxH` changes the resolution of a running Android VM on the fly (restarts the compositor session, not the VM; the change lasts until the next boot replay of fw_cfg).
- **Android display resolution 1920x1080**: `andler-waydroid-compositor` now appends `[output] name=Virtual-1 mode=…` to the base weston.ini at startup, defaulting to `1920x1080` (verified present in the virtio-gpu EDID mode list). The generated config is written into the session's `XDG_RUNTIME_DIR` (the unit runs as `User=user`; `/run/andler` is root-only and a first attempt there failed every boot with `mkdir: Permission denied`, crashing the weston session in a restart loop). The instance config's `display.resolution` is passed into the VM as QEMU fw_cfg (`opt/andler/display-resolution`) and applied by a guest oneshot (`andler-display-resolution.service`) to `/etc/andler/display.conf`, which `waydroid-compositor.service` loads via `EnvironmentFile=` — so the resolution chosen at instance creation is what weston applies, and a static `RESOLUTION=WxH` in `/etc/andler/display.conf` overrides it. The Linux VM (KDE Plasma) exposes the same EDID modes; see `docker/images/README.md`.
- **`rtkit` package added to the base image** — silences pipewire's boot-time `mod.rt: RTKit error: ServiceUnknown` spam; RTKit is D-Bus-activated on demand so realtime scheduling actually applies to the guest session.
- **No more double session starts**: the pipewire units (`andler-pipewire.service`, `andler-pipewire-pulse.service`) now order themselves `After=systemd-user-sessions.service`. Their `PAMName=login` PAM stack includes pam_nologin, which rejects logins while `/run/nologin` exists — early in boot the units failed once ("System is booting up…"), `Restart=on-failure` bounced them, and since `waydroid-compositor.service` `Requires=` the pulse unit, the whole weston+waydroid session restarted mid-start (two westons, a second container start, and the composer@2.1-se abort inside the interrupted first boot). First start now succeeds.
- **`andlerd` survives KDE logout**: the daemon is a per-user systemd unit (`scripts/install.sh`); with `loginctl enable-linger` it keeps running after the desktop session ends, so `andler list` keeps working. `install.sh` now resolves the binary to an absolute path (a relative one made systemd reject the unit with "bad unit file setting").
- **`scripts/uninstall.sh`**: removes the per-user unit (stop + disable + delete + `daemon-reload`); `--purge` also deletes `~/.andler` data (with an explicit TTY confirmation, mirroring `andler remove --purge`) and the `/etc/sudoers.d/andler` rules. Never deletes the andlerd binary or touches other users.

- **Android session compositor swapped: gamescope → Weston**: the Android session (`waydroid-compositor.service`) now runs Weston (DRM backend, kiosk shell) instead of gamescope. Gamescope presents exclusively through Vulkan, and on NVIDIA hosts the venus device exposes no DRM format modifiers for scanout formats, so every frame import into KMS failed (`Cannot import FB … not supported for scan-out`) and the instance booted to a black screen with a silent crash loop. Weston composites through GL (virgl) — the same path the Linux instance's KDE session uses — and presents on any host GPU. venus stays enabled on the virtio-gpu device for guest Vulkan apps. See `docker/images/README.md` for the full rationale.
- **Snapshot metadata persistence** (`andler-store`): `snapshots` table with `ON DELETE CASCADE` from `instances`. Full CRUD for snapshot metadata.
- **gRPC snapshot operations** (`andler-rpc`): `CreateSnapshot`, `RestoreSnapshot`, `DeleteSnapshot`, `ListSnapshots` RPCs with per-operation timeout support.
- **gRPC metrics streaming** (`andler-rpc`): `StreamResourceMetrics` server-streaming RPC with `ResourceMetricsResponse` (all 9 optional fields including GPU).
- **gRPC config editing** (`andler-rpc`): `UpdateInstanceConfig` RPC with `UpdateInstanceConfigRequest` mirroring `GetInstanceConfigResponse` fields.
- **NVML integration** (`andler-firmware`): `nvml-wrapper` crate for NVIDIA GPU metrics (primary), with `nvidia-smi` CLI fallback.
- **Hardware auto-detection** (`andler-firmware`): `detect_all()` returns `HardwareDefaults` — GPU render backend, display engine, audio server, ARM translator, OVMF paths, Venus support, passt availability.
- **Network configuration service** (`andler-net`): `NetworkService` trait and `DefaultNetworkService` implementation for bridge network mode, using `iproute2` for host-side network setup. `NetworkMode::Isolated` is accepted by config but `setup_isolated` returns an explicit "not implemented yet" error (later documented in `services/andler-net/README.md`).
- **Guest image pipelines** (`docker/images/`): Automated Android/Linux base image builds with Waydroid. `base/Dockerfile` builds Arch Linux rootfs with CachyOS kernel, Mesa/Venus, waydroid, weston, UKI. `build.sh` orchestrates docker build + disk conversion. `build-disk.sh` converts rootfs to GPT-partitioned bootable qcow2 (ESP + ext4 + UKI). `fetch-waydroid-images.py` downloads system.img/vendor.img from SourceForge with MD5 verification and disk space pre-check. Supports Android 11 (LineageOS 18.1) and 13 (LineageOS 20.0), VANILLA/GAPPS variants.
- **Base image auto-discovery** (`andler-core`): `base_image::resolve()` scans `~/.andler/cache/base-images/` for `*.manifest.json` files, picks freshest match by (android_major, variant). Daemon uses it automatically when client omits `base_image_path`. Error message names the exact `docker/images/build.sh` invocation to produce a missing image.

#### Daemon

- **Snapshot orchestration**: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots` methods with FSM state validation and per-operation timeout override.
- **Health checks**: periodic background task (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s) polls every `Running` instance's real backend status; a process that died outside `stop_instance` is transitioned to `Error` and persisted instead of going unnoticed. Auto-restart deliberately not included — see `health_ops.rs` module doc for why (the FSM currently has no supported way to restart the *same* instance record once `Stopped`/`Error`, a pre-existing constraint this surfaced, not something this change could safely work around).
- **Metrics streaming**: `stream_resource_metrics` method returning `BoxStream<'static, ResourceMetrics>`.
- **Factory reset / `remove --purge`**: Full end-to-end with file cleanup (disk + OVMF vars + instance directory). Refuses when live `Linked` clones exist.
- **Clone for LinuxVm**: `clone_instance` supports `Linked` and `FullStandalone` modes.
- **Export for LinuxVm**: `export_instance_disk` works for both AndroidVm and LinuxVm.
- **Linux VM creation**: `create_linux_instance` creates instance directory, provisions OVMF VARS, creates qcow2 disk, registers instance.
- **Config editing**: `update_instance_config` replaces instance config (protects id, kind, disk.path).
- **Partial instance ID** (Docker-style): `resolve_instance_id` resolves 8-char hex prefixes, rejects ambiguous/non-hex input. `--full-id`/`-q` flag on `list`.
- **`write_instance_toml`**: Writes `instance.toml` alongside instance on create/clone.
- **`spawn_compact_on_shutdown`**: Background compaction task when `compact_on_shutdown = true`.
- **`purge_instance_files`**: Uses `remove_dir_all` for UUID-pattern instance directories.
- **Signal handling**: SIGINT + SIGTERM graceful shutdown (stops all running instances).
- **systemd user unit**: `scripts/andlerd.service` with `scripts/install.sh`.
- **Snapshot limit**: `MAX_SNAPSHOTS_PER_INSTANCE = 20` with `SnapshotLimitExceeded` error.
- **New error variants**: `SnapshotLimitExceeded`, `MalformedInstanceRef`, `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged` (DaemonError: 29 variants total).
- **gRPC round-trip tests**: 28 integration tests with real TCP connections.

#### CLI

- **Unified `create` command**: Single command with `--kind linux`/`--kind android` discriminator. TOML mode auto-detects type from content.
- **Linux VM CLI args**: `--kind linux --name --iso-path --disk-path --ovmf-vars-template [--disk-size-gib]` creates LinuxVm without TOML.
- **Android VM from TOML**: `--file android.toml` with `android_version` field creates AndroidVm — no CLI flags needed.
- **Snapshot `--timeout` flag**: Override per-instance snapshot timeout for a single operation.
- **`andler disk` commands**: `create`, `info`, `resize`, `compact` for disk management. Flexible size format (`64GB`, `128000MB`, `1T`).
- **`disk resize --shrink`**: Explicit confirmation required for shrinking disks.
- **Metrics display**: GPU columns (VRAM, GPU%) when AMD/NVIDIA/Intel data available. Human-readable byte formatting.
- **Snapshot subcommands**: `create`, `restore`, `delete`, `list` under `andler snapshot`.
- **Guest package management**: `andler guest install/remove/list` — install, remove, or list known packages (`spice-vdagent`, `qemu-guest-agent`, `spice-webdavd`) in guest OS. Auto-fallback: online via the guest agent socket (`*.qga.sock`) if VM running, offline via `qemu-nbd` + mount if stopped. `install libndk|libhoudini [--translator-dir <path>]` switches the ARM translator instead of using a package manager.
- **`clone` and `export`**: Commands for LinuxVm + AndroidVm.
- **Default paths**: Instance data stored under `~/.andler/` by default.
- **`edit` command**: Open instance config in `$VISUAL`/`$EDITOR` as TOML, apply changes via gRPC.
- **`wizard` command**: Interactive VM creation wizard (also default when `andler` invoked without subcommand). Basic/Advanced modes, hardware auto-detection summary.
- **`completions` command**: Generate shell completion scripts (bash/zsh/fish) via `clap_complete`.
- **`list` filtering/sorting**: `--state`, `--name` (regex), `--sort` (name/state), `--json`, `--full-id`/`-q`.
- **`logs` filtering**: `--source` (stdout/stderr), `--grep` (regex), `--tail` (backlog lines).
- **`metrics` output modes**: `--once` (single sample), `--json` (machine-readable).
- **`create --quick`**: Skip wizard, create with all defaults. Requires `--kind`.
- **`--arm-translator`**: Replaces `--libndk` boolean. Values: `none`, `libndk`, `libhoudini`.
- **Colored status output**: `colorize_status()` with ANSI codes gated on `IsTerminal`.
- **Error formatting**: `format_grpc_error()` — human-readable gRPC error messages.
- **"Daemon not running" error**: `looks_like_daemon_not_running()` — friendly message with startup hint.
- **Instance ID echo**: `resolve_echo()` — prints full ID + name after lifecycle operations.
- **Snapshot spinner**: `indicatif` spinner during snapshot create/restore (hidden when not a terminal).
- **CLI-side validation**: `validate_linux_paths`/`validate_android_paths` — checks existence before gRPC call.
- **`create --dry-run`**: Prints the resolved instance config and QEMU command line without contacting the daemon at all — client-side resolution mirrors the daemon's own logic (OVMF auto-detection, fresh-disk path relocation, `andler_qemu::cmdline::build_args`). Works in TOML mode and CLI mode; not supported with a bare `andler create` (the wizard already shows a summary before creating).
- **`create --verify`**: Validates a resolved instance config (paths exist, OVMF found/required-for-Android, disk size sane, GPU memory/CPU/memory in range) and prints a ✓/✗ report without contacting the daemon; exits non-zero if any check fails. Shares the same client-side resolution as `--dry-run` (`apps/cli/src/preview.rs::resolve_linux`/`resolve_android`).

### Changed

- **Monorepo restructure**: `crates/andler-*` reorganized into grouped directories — `core/`, `backends/`, `services/`, with the `daemon`/`cli` applications under `apps/`. Package names keep `andler-` prefix.
- **Documentation language**: All docs now in English. Historical/future docs moved to `docs/archive/`.
- **Daemon module decomposition**: `apps/daemon/src/daemon.rs` (3209 lines) decomposed into 9 files under `apps/daemon/src/daemon/`. Core `mod.rs` reduced to 268 lines (92% reduction). Error types, instance lifecycle, clone/export, snapshots, and queries each in separate modules. Tests split into 9 domain-specific test files.
- **CLI module decomposition**: `apps/cli/src/main.rs` (1185 lines) decomposed into 8 modules. Main dispatch reduced to 769 lines. Commands extracted to domain-specific files: `create.rs`, `edit.rs`, `status.rs`, `snapshot.rs`, `disk.rs`, `lifecycle.rs`, `clone.rs`, `helpers.rs`.
- **Disk default size**: 40 GiB → **256 GiB** (thin-provisioned qcow2, actual usage minimal).
- **Database filename**: `state.db` → **`andlerd.db`**.
- **Snapshot state requirements**: `restore`/`delete` now require **Running/Paused** instance (not terminal states) — QMP commands need live QEMU process.
- **`--libndk` → `--arm-translator`**: Boolean flag replaced by enum: `none`, `libndk`, `libhoudini`.
- **DaemonError expanded**: 14 → **29 variants** (added SnapshotLimitExceeded, MalformedInstanceRef, ConfigIdMismatch, ConfigKindChanged, ConfigDiskPathChanged, EmptyInstanceRef, InstanceRefNotFound, AmbiguousInstanceId, and more).
- **Environment variables**: Added `ANDLERD_LISTEN_ADDR` (daemon listen), `ANDLERD_OVMF_CODE`/`ANDLERD_OVMF_VARS` (firmware override), `ANDLERD_LOG_FORMAT` (json output). `ANDLERD_ADDR` remains for CLI client.
- **`stop --graceful` semantics**: `--graceful` sends SIGTERM and waits for graceful ACPI shutdown. Default (without flag) is force kill via SIGKILL.
- **`purge_instance_files`**: Uses `remove_dir_all` for UUID-pattern instance directories (was `remove_dir`, silently failed on non-empty).
- **QemuBackend pause/resume**: Lock extracted before `backend.pause().await` (was held across await, blocking all operations).
- **Metrics output format**: `key=value` pairs (was columnar table headers).
- **Venus default on NVIDIA**: Skips Mesa version check for NVIDIA vendor (Venus uses Vulkan, not Mesa's OpenGL).
- **Clipboard documentation**: Wizard prints `spice-vdagent` install note when clipboard is enabled.
- **Resolution documentation**: Help text and doc comments note this isn't applied to SDL/GTK output yet.
- **Instance config persistence**: `write_instance_toml` writes TOML alongside instance (SQLite remains source of truth for restore).

#### CLI (`cli`)

- **`--json` on `status` and `snapshot list`**: `andler status <id> --json` emits `{instance_id, state, detail, error_message}`; `andler snapshot --json list <id>` emits a JSON array. Other commands unchanged (list/metrics already had `--json`).
- **Destructive-op confirmations**: `remove --purge` and `snapshot delete` ask for confirmation on interactive terminals (`n` → `Cancelled.`, nothing deleted). Scripted (non-TTY) runs keep the old silent behavior.
- **Snapshot timestamps**: `created_at` shown as local `YYYY-MM-DD HH:MM:SS` instead of raw RFC3339.
- **Wizard name validation**: instance names restricted to `[a-zA-Z0-9][a-zA-Z0-9_-]*` (names feed file paths).
- **Wizard ISO validation**: path must end in `.iso` or `.img`.
- **Wizard summary**: Linux VMs show `Firmware: UEFI/OVMF | Legacy BIOS`; network label `NAT/passt` → `NAT (passt)`; Android name placeholder `my-android-vm`.
- **`guest list` ANSI colors**: colored status codes only when stdout is a TTY (was always emitted).
- **Clap help texts**: doc-comment help on every subcommand and `--purge`/`--graceful`/`--quick`/`--dry-run`/`--verify`/`--json` flag help.
- **`config edit` edits the real `instance.toml`**: opens the actual on-disk file in `$VISUAL`/`$EDITOR` (fallback `vi`/`vim`/`nano`) instead of a throwaway temp copy; invalid edits are rejected with the file path to fix, and nothing is applied.
- **`disk create --size 0` rejected** and `--disk-size-gib 0` rejected by clap (`range(1..)`).
- **`InstanceConfig::validate()`** (`andler-core`): cross-field sanity checks (nonzero cores/memory/disk, memory limits, GPU hostmem limits); CLI flag values enforce ranges up front.
- **E2E smoke test**: negative checks added — `status` of a nonexistent instance must fail, `disk create --size 0` must be rejected, `create --disk-size-gib 0` must be rejected.

#### gRPC (`andler-rpc`)

- **`CdromBus` no longer defaults**: `CreateInstanceRequest.cdrom_bus = UNSPECIFIED` is rejected with `INVALID_ARGUMENT` (`ConvertError::MissingField("cdrom_bus")`) instead of silently falling back to `IDE`. Clients must set the field explicitly; the daemon→proto direction still maps stored configs (none predating the field exist in practice).

#### Guest provisioning (offline + online)

- **Chroot environment for offline package operations**: `bind_host_mounts` now prepares the mounted guest for real package-manager runs — the guest's `/etc/resolv.conf` is *written* with the host's nameservers via NOPASSWD `sudo chroot` (a bind-mount fails with ENOENT on the dangling `stub-resolv.conf` symlink Arch images ship), `/dev`, `/proc`, `/sys` are bind-mounted, and a fresh tmpfs is mounted on the guest's `/run` (pacman's gpg-agent otherwise fails with `GPGME error: Invalid crypto engine` — the on-disk `/run` is unwritable). All done inside `mount_partition`; failures degrade to warnings with `tracing::warn!`.
- **Package index refresh before offline install**: `install_agent_offline_blocking` runs the manager's index update first (`apt-get update` / `dnf makecache` / `pacman -Sy`) so installs succeed on fresh images; on update failure the guest's `/etc/resolv.conf` content (or its absence) is logged for diagnosis and the error is reported (exit status + stderr).
- **Online guest operations moved to the guest-agent chardev**: guest package install/remove and guest file writes now talk to QGA over the dedicated `*.qga.sock` chardev (`virtserialport name=org.qemu.guest_agent.0`, added by `cmdline::guest_agent_args`) — not the QMP monitor, because QEMU ≥ 9 no longer registers `guest-*` commands on QMP. `backend::guest_agent_client` connects lazily; `guest_exec_package` detects the package manager inside the guest with `command -v apt-get|dnf|pacman` and dispatches. Requires `qemu-guest-agent` running in the guest (base image ships it enabled).
- **`andler guest install libndk|libhoudini <id> [--translator-dir <path>]`**: ARM-translator installs are special-cased to `SwitchArmTranslator` (offline disk staging) instead of the package-manager path; `--translator-dir` points at a local extracted cache to skip the download.
- **ARM translator switch is atomic**: new files are staged into `system/.andler-translator-staging` first (with verification), the old translator is removed and the staged files renamed into place only after staging succeeds; `build.prop` is merged and written sorted.
- **Android boot-mode switch via chroot symlink**: `switch_boot_mode` re-points the guest's `default.target` with `sudo -n chroot ln -sfn` (the guest filesystem is root-owned and the daemon is unprivileged; direct writes were `Permission denied`).

#### Daemon

- **`config set` key whitelist**: supported keys are `display.resolution` (any state), `name` and `arm_translator` (stopped instance). `display.resolution` accepts `WxH`; on a `Running`/`Paused` instance it is applied live through the guest agent (`set_guest_display_resolution` → guest-side `display.conf` + compositor/session apply) and persisted for the next boot via fw_cfg. Unknown keys are rejected with `InvalidConfigKey` (`INVALID_ARGUMENT`).
- **Live display resolution change**: new `HypervisorBackend::set_guest_display_resolution` writes `/etc/andler/display.conf` in the guest via QGA `guest-file-*` and triggers the guest-side applier (`andler-set-resolution` on Android, session apply on Linux) — the VM keeps running.

### Fixed

- **Daemon metrics poller panicked on decreasing `/proc/<pid>/io` counters**: `compute_io_rates` subtracted u64 counters that can go down (page-cache accounting is not strictly monotonic) — `attempt to subtract with overflow` killed the metrics task mid-session. Counters now clamp to zero via `saturating_sub`; regression-covered by `compute_io_rates_clamps_decreasing_counters_to_zero`.
- **Translator install shadowed the base `build.prop` and broke Android boot (found live)**: the upper overlay `build.prop` was written with only the 10 translator props, hiding the base image's full `build.prop` wholesale — waydroid then failed to parse the Android version (`invalid literal for int() with base 10: ''`) and ART's `derive_classpath` aborted, so the container never finished booting. The merge now always regenerates the upper from the base below the overlay: a plain `system/build.prop`, or `/system/build.prop` extracted with `debugfs` from `etc/waydroid-extra/images/system.img` (e2fsprogs is essential on Debian/Arch and needs no root). The existing upper is never read back, so an already-broken install is repaired by re-running `guest install` (or `config set arm_translator none`); without any base source the install fails loudly instead of shipping a broken upper. Regression-covered by `base_build_prop_*`/`parse_build_prop_*` unit tests and a new E2E assertion that the upper merges fixture `system.img` props with translator props.
- **`arm_translator.rs` build break**: `for file in &info.files` iterated `&&[&str]` (not an iterator) — the workspace didn't compile. Fixed by dropping the `&`.
- **`instance_file.rs` panics on missing fields**: `into_request()` used `.expect()` on `disk_path`/`iso_path` — a TOML file missing either field panicked. Now returns `InstanceFileError::MissingField` (`missing required field disk_path in instance file`).
- **Unknown `android_version` silently downgraded**: a TOML `android_version = 12` (or anything but 11/13) was silently created as Android 13. Now rejected with `unsupported value for android_version: 12`.
- **Wizard overlay size**: Android wizard took `overlay_size_bytes` from `disk_size_gib` (a 256 GiB disk → 256 GiB overlay). Now a fixed 20 GiB default, matching the TOML path.
- **`ask_network_mode` ignored `prefilled`**: the network-mode prompt always started on NAT even when the prefilled value was Bridge/Isolated. Now honors the prefilled cursor position.
- **`compact()` temp-file leak**: a failed `rename` of the temporary compacted disk left the temp file behind. Now removed on the error path.
- **`guest_tools` ignored `apt-get update`/`dnf`/`pacman` failures**: a failed package-index update was treated as success. Now fails with the exit status and stderr.
- **`is_agent_installed` masked I/O errors**: a failed `chroot`/binary check returned `false` (→ "install" path) instead of an error. Now returns `Result<bool, DiskError>`.
- **`nbd_status` swallowed scan errors**: a missing/inaccessible `/sys/block` produced a silently empty status. Now returns `Result<NbdStatus, DiskError>`; `andler doctor` reports a failed scan with `Run: sudo modprobe nbd max_part=8`.
- **Disk flag form ignored extra flags**: `andler disk --create --info ...` silently ran `--create`. Now rejects multiple actions (`disk: actions are mutually exclusive, got --create and --info`) and requires at least one.
- **`parse_size("0")` accepted for disk create**: a 0-byte disk image was created. Now rejected (`disk: refusing to create a 0-byte disk image`).
- **Metrics poller blocked the async runtime**: synchronous `/proc`/sysfs reads ran inline in the poller task. Now collected in `tokio::task::spawn_blocking`.
- **`create` output lacked the instance name**: `Created instance <name> (<id>)` in all non-wizard modes.
- **Duplicate help text in `ask_display_resolution`**: the "not yet applied to the actual display output" note was printed twice.

- **`create_instance` (Linux) silently discarded `--ovmf-vars-template`**: the
  gRPC handler parsed the client's `firmware.ovmf_vars_path` into `InstanceConfig`
  but then unconditionally passed the daemon's own auto-detected
  `self.ovmf.vars_template` to `create_linux_instance`, overwriting it — an
  explicit `--ovmf-vars-template` had no effect. `create_android_instance`
  already had the correct fallback order (client value, else auto-detected);
  the Linux path now matches it. Found while implementing `--dry-run` (the
  preview needed to replicate this exact fallback logic client-side, which
  is what surfaced the mismatch). Covered by
  `create_instance_honors_explicit_ovmf_vars_template` in
  `apps/daemon/src/grpc_roundtrip_test.rs`.
- **`wait_job_completion` terminal status check**: was matching on `"completed"`/`"failed"`/
  `"aborted"`, none of which exist in QEMU's real job status enum (the only terminal status is
  `"concluded"`; success/failure is distinguished by the presence of an `error` field, not by a
  separate status value). This meant every snapshot operation — even a successful one — would
  poll until timeout rather than ever detecting completion. Fixed, and `job-dismiss` is now
  called after a job concludes (previously never called, leaving concluded jobs visible in
  `query-jobs` forever). This machinery is now only reachable via `wait_job_completion` as a
  generic QMP utility — snapshot paths use the synchronous disk-only API (see Changed).
- **`execute_raw` no longer errors on async QMP events** (e.g. `JOB_STATUS_CHANGE`) received
  between sending a command and reading its reply — these can legitimately interleave with
  command/response traffic during job polling. Previously any such message was treated as a
  parse error.
- None of the above were caught by the existing test suite — `wait_job_completion`'s tests
  encoded the same incorrect status strings as the implementation. New tests cover the real
  status semantics over a `UnixStream::pair`-based fake QMP peer (see `andler-qemu/src/qmp.rs`).
- **`andler-qemu` Intel GPU metrics**: the sysfs path used for GPU load
  (`device/gt/gt0/attrs/busyiffies`) and the two used for VRAM
  (`mem_info_dev_local_mem_alloc`, `mem_info_stolen_local_mem`) do not exist anywhere in the real
  i915 sysfs tree — confirmed against `i915_sysfs.c` and the upstream `gt/` sysfs reorganization.
  On real Intel hardware this silently returned `None` for both metrics, with no error, and no
  test exercised the path string itself (only the delta arithmetic, with hand-picked numbers).
  Fixed to read `device/power/rc6_residency_ms` (a real, documented, long-standing i915 ABI) for
  GPU load, computed from a real elapsed-time delta (`Instant`) rather than an assumed fixed
  1-second polling interval. VRAM is now honestly `None` for Intel rather than read from
  nonexistent paths — there is no equivalently simple, stable `sysfs` ABI for it (stolen-memory
  accounting lives in `debugfs`).
  plain `SystemTime::now().duration_since(...)` (entirely safe Rust; the `unsafe` did nothing and
  the accompanying safety comment justified nothing real). `NbdGuard`/`MountGuard`'s `Drop` impls
  previously discarded the result of `qemu-nbd --disconnect`/`umount -l` entirely (`let _ = ...`)
  — neither a failed spawn nor a non-zero exit status was ever observed, which could leave
  `/dev/nbd*` devices connected indefinitely with no diagnostic trail. Both now log via
  `tracing::warn!` on failure (added `tracing` as a dependency of `andler-disk`, which it
  previously lacked). Also replaced `.to_str().unwrap()` with `.to_string_lossy()` in both `Drop`
  impls so a non-UTF-8 path can't turn an already-failing cleanup into a panic during unwind.
  Separately, found and fixed stray CJK characters embedded in Russian-language doc comments
  in `metrics.rs` — encoding/generation artifacts, not intentional text.

- **gRPC client died with "h2 protocol error" on long error messages**: multi-KB package-manager stderr (e.g. `pacman -Sy` output) sent as the `grpc-message` header tripped tonic's h2 client (`internal error: h2 protocol error: http2 error`) on the trailers-only response path; short errors like "instance not found" worked. `status_message()` now replaces control characters (other than HTAB) with spaces and truncates the message to 384 chars before building the `Status`. Verified end-to-end: the same response that previously produced the h2 error now prints a readable message.
- **Offline install DNS (`Could not resolve host`)**: the first fix attempt wrote the guest `/etc/resolv.conf` with bare IP addresses (`127.0.0.53`), which resolv.conf ignores — `host_nameservers()` now emits full `nameserver <ip>` directives (deduplicated, sourced from `/etc/resolv.conf` or `/run/systemd/resolve/stub-resolv.conf`). Offline `pacman -Sy` resolves mirrors correctly after this.
- **`handler-disk find_live_clones` removed from the disk crate**: the live-clone scan moved to the daemon level (`Daemon::find_live_clones`, scanning registered instances for `config.disk.base_image == source`); the old module-level implementation was deleted, and `remove --purge` protection (`InstanceHasLiveClones`) is unchanged.

### Removed

- Dead stub crates: `frontend/`, `guest-image/`, `packaging/`, `presets/`
- Dead `--instance-kind` flag from CLI
- `create-android` command (merged into `create`)
- Russian-language documentation (moved to `docs/archive/`)
- `--libndk` boolean flag (replaced by `--arm-translator` enum)

## [0.1.0] — Pre-Release

### Added

- Initial domain model (`andler-core`): `InstanceConfig`, `HypervisorBackend` trait, FSM, `CloneMode`, `AndroidProfile`
- QEMU backend (`andler-qemu`): Process management, QMP client, command-line builder
- Disk operations (`andler-disk`): QCOW2 creation/overlay/clone/resize via `qemu-img`
- State store (`andler-store`): SQLite persistence for instances
- gRPC protocol (`andler-rpc`): Proto definitions and conversions
- Daemon (`andler-daemon`): Instance lifecycle management, persistence, log streaming
- CLI (`andler-cli`): Thin gRPC client with all instance operations
- Docker build/test infrastructure
