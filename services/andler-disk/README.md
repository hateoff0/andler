# andler-disk

Disk operations for virtual machines: creation, cloning, resizing, compaction — a wrapper around `qemu-img`, plus domain-specific overlay disk logic for Android instances, and offline guest tools provisioning via `qemu-nbd`.

## Modules

### `qcow2` — QCOW2 Operations

All functions are `pub async`, wrapping `qemu-img` CLI invocations via `tokio::process::Command`.

| Function | Signature | Description |
|----------|-----------|-------------|
| `create` | `(path: &Path, size_bytes: u64) -> Result<(), DiskError>` | Create a new standalone qcow2 image. Corresponds to `qemu-img create -f qcow2`. |
| `create_with_backing_file` | `(path: &Path, backing_file: &Path, size_bytes: u64) -> Result<(), DiskError>` | Create a qcow2 overlay with backing file. Checks backing file existence before spawning qemu-img. Corresponds to `qemu-img create -f qcow2 -F qcow2 -b`. |
| `clone_full` | `(source: &Path, dest: &Path) -> Result<(), DiskError>` | Full independent clone via `qemu-img convert`. Preserves thin-provisioning. |
| `resize` | `(path: &Path, new_size_bytes: u64, allow_shrink: bool) -> Result<(), DiskError>` | Change logical size. Corresponds to `qemu-img resize`. Shrinking (`new_size_bytes` < current) without `allow_shrink = true` returns `DiskError::ShrinkRequiresConfirmation` instead of touching the file — see API.md, "Disk management". |
| `compact` | `(path: &Path) -> Result<(), DiskError>` | Remove free blocks via `qemu-img convert` to temp file + atomic rename. qcow2-only — returns `DiskError::CompactNotApplicable` for other formats (e.g. raw has no reclaimable metadata). |
| `virtual_size_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read logical (virtual) size from `qemu-img info --output=json`. |
| `disk_usage_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read actual disk usage from `qemu-img info --output=json` (`actual-size` field). |
| `info` | `(path: &Path) -> Result<DiskInfo, DiskError>` | Read full disk metadata (format, virtual size, actual size, backing file) from `qemu-img info --output=json`. |

No dependency on `andler-core` — works with `&Path` directly, knows nothing about `InstanceConfig`.

### `overlay` — Android Overlay Disks

Domain-specific layer over `qcow2::create_with_backing_file` for Android instances.

| Function | Signature | Description |
|----------|-----------|-------------|
| `create_overlay` | `(instance_dir: &Path, base_image_path: &Path, overlay_size_bytes: u64) -> Result<OverlayDisk, DiskError>` | Creates overlay at `<instance_dir>/disk.qcow2`. Returns `BackingFileNotFound` if base image doesn't exist. |
| `factory_reset` | `(instance_dir: &Path, base_image_path: &Path, overlay_size_bytes: u64) -> Result<OverlayDisk, DiskError>` | Deletes existing overlay and recreates from base. Instance must be stopped first. |

**`OverlayDisk`**: `overlay_path` + `base_image_path` (for logging/diagnostics).

`factory_reset` is delete + recreate, not "clear" of the existing file.

### `clone` — Instance Disk Cloning

Three modes for cloning an existing instance's disk into a new disk (not from a shared `base_image` like `overlay.rs` — separate module because of the different backing chain semantics):

| Function | Signature | Description |
|----------|-----------|-------------|
| `linked_clone` | `(source_disk_path: &Path, dest_path: &Path, size_bytes: u64) -> Result<ClonedDisk, DiskError>` | Overlay with `backing_file = source_disk_path`. Cheap/fast (copies no data). Creates dependency: source cannot be purged while linked clones exist. |
| `full_standalone_clone` | `(source_disk_path: &Path, dest_path: &Path) -> Result<ClonedDisk, DiskError>` | Flattens entire backing chain into independent file via `qemu-img convert`. Expensive but fully independent. |
| `shared_base_clone` | `(source_disk_path: &Path, dest_path: &Path, source_base_image: &Path) -> Result<ClonedDisk, DiskError>` | Byte-copy of source disk file (`tokio::fs::copy`). Inherits qcow2 metadata/backing_file, remains thin, physically independent from source. Survives source deletion. |

**`ClonedDisk`**: `disk_path` + `backing_file: Option<PathBuf>` (`None` for full standalone, `Some(source)` for linked, `Some(shared_base)` for shared base).

**Lifecycle:** Live-clone tracking lives at the daemon level, not here — `Daemon::find_live_clones` (in `andler-daemon`) scans registered instances for `config.disk.base_image == source` and blocks purge of a source disk that still has linked clones (`InstanceHasLiveClones`). The disk crate only knows how to *create* the three clone shapes.

### `guest_tools` — Offline Guest Package Management

Checks and manages packages in guest OS filesystems via `qemu-nbd` + mount + chroot.

| Function | Signature | Description |
|----------|-----------|-------------|
| `detect_package_manager` | `(mount_point: &Path) -> Option<PackageManager>` | Detect package manager from binary presence (`usr/bin/apt-get` → Apt, `usr/bin/dnf` or `usr/bin/yum` → Dnf, `usr/bin/pacman` → Pacman) |
| `is_agent_installed` | `(mount_point: &Path, pm: PackageManager, package: &str) -> Result<bool, DiskError>` | Check installation via the manager's query (`dpkg -l` / `rpm -q` / `pacman -Q`), run inside the chroot through the helper (`chroot-run`) |
| `install_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Install package offline (NBD connect → mount → index refresh → chroot install). Wrapped in `spawn_blocking`. |
| `remove_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Remove package offline (NBD connect → mount → chroot remove). Wrapped in `spawn_blocking`. |
| `check_package_status_offline` | `(mount_point: &Path, binary_checks: &[&str]) -> PackageStatus` | Check binary presence in mounted filesystem — any candidate path (`/usr/bin/...` and `/usr/sbin/...`) marks the package installed |
| `check_all_packages_offline` | `(mount_point: &Path) -> Vec<(&GuestPackage, PackageStatus)>` | Check all KNOWN_PACKAGES in mounted filesystem |
| `check_all_packages_offline_with_disk` | `(disk_path: &Path) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | Full offline check: NBD connect + mount + check + unmount |
| `check_android_packages_offline_with_disk` | `(disk_path: &Path) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | Full offline check for Android packages: NBD connect + mount + check + unmount |
| `available_packages` | `(kind: &InstanceKind) -> &'static [GuestPackage]` | Return package list for instance kind: `ANDROID_PACKAGES` for Android, `KNOWN_PACKAGES` for Linux |

**`PackageManager`**: `Apt` | `Dnf` | `Pacman`, with `binary_name()`, `install_args(pkg)`, `remove_args(pkg)`, `check_installed_command(pkg)` helpers.

**`PackageStatus`**: `Installed` | `NotInstalled` | `Unknown` (the check scripts can exit non-zero for reasons other than "not installed").

**Offline install flow**: connect NBD → wait for partitions → mount root partition → detect package manager → refuse if already installed (`AgentAlreadyInstalled`) → refresh indexes first (`apt-get update` / `dnf makecache` / `pacman -Sy`) so installs succeed on fresh images → chroot install. An index-refresh failure is a hard error (reported with exit status + stderr via `describe_helper_failure`), and the guest's `/etc/resolv.conf` content (or its absence / dangling-symlink metadata) is logged at `info` for DNS diagnosis. All privileged steps run via NOPASSWD `sudo -n chroot <mount> ...`.

**Known Packages** (`KNOWN_PACKAGES`): `spice-vdagent` (`/usr/bin/spice-vdagentd`), `qemu-guest-agent` (`/usr/bin/qemu-ga`), `spice-webdavd` (`/usr/bin/spice-webdavd`).

**Android Packages** (`ANDROID_PACKAGES`): `libndk` (`var/lib/waydroid/overlay/system/lib/libndk_translation.so`), `libhoudini` (`var/lib/waydroid/overlay/system/lib/libhoudini.so`).

### `boot_mode` — Android Boot Mode Switching

Repoints the guest's `etc/systemd/system/default.target` symlink between the Android and Linux target units on a mounted disk.

| Function | Signature | Description |
|----------|-----------|-------------|
| `switch_boot_mode` | `(overlay_path: &Path, mode: AndroidBootMode) -> Result<(), DiskError>` | NBD connect → mount → verify target unit exists → `sudo -n chroot ln -sfn` the `default.target` link. Fails with `BackingFileNotFound` if the disk doesn't exist. |
| `current_boot_mode` | `(overlay_path: &Path) -> Result<AndroidBootMode, DiskError>` | Read the current target from the mounted disk (`default.target` → Android target ⇒ Android, anything else ⇒ Linux; error if the link is missing). |

The guest's `/etc/systemd/system` is root-owned 755, so the mutation itself must go through `sudo -n chroot` (a direct `std::fs` write fails with EPERM even though the mount is rw); `ln -sfn` both removes the old symlink and creates the new one in one step.

### `diskspace` — Free Disk Space Pre-check

Checks free space on a path's filesystem via `statvfs(2)` before an operation that could otherwise fail partway through with a raw ENOSPC — see ROADMAP.md, "Core: add disk space pre-check before snapshot operations".

| Function | Signature | Description |
|----------|-----------|--------------|
| `check_available_space` | `(path: &Path, required_bytes: u64) -> Result<(), DiskError>` | Fails with `InsufficientDiskSpace` if fewer than `required_bytes` are free on `path`'s filesystem (resolved to its nearest existing ancestor if `path` doesn't exist yet) |

Uses `f_bavail` (blocks available to an unprivileged user), not `f_bfree` (which includes root-reserved blocks the daemon may not actually be able to use, e.g. ext4's default 5% reservation).

**Rationale:** QCOW2 snapshots grow the same disk file — a nearly-full filesystem can fail mid-write. `required_bytes` is a conservative estimate that accounts for worst-case qcow2 internal fragmentation during snapshot creation.

### `nbd` — NBD Device Management

Manages QEMU NBD (Network Block Device) connections for mounting disk images without writing to them.

| Function | Signature | Description |
|----------|-----------|-------------|
| `nbd_status` | `() -> Result<NbdStatus, DiskError>` | Report whether the NBD module is loaded and how many `/dev/nbd*` devices are free/total. Never loads the module itself. |
| `find_free_nbd_device` | `() -> Result<PathBuf, DiskError>` | Find a free `/dev/nbd*` device. Best-effort `modprobe nbd max_part=8` first (`try_autoload_nbd_module`); fails with `NbdSetupFailed` if no NBD kernel module loaded or no free device. |
| `connect_nbd` | `(overlay_path: &Path) -> Result<NbdGuard, DiskError>` | Connect an overlay disk to an NBD device via `qemu-nbd --connect`. Takes an exclusive `flock` on a per-disk lock file **before** picking a device (otherwise two processes could each grab a different free `/dev/nbd*` and both connect to the same disk); the lock is held for the guard's whole lifetime. Returns `NbdGuard` (RAII: disconnects + releases the lock on drop). |
| `helper_command` | `(subcommand: &str) -> Command` | Build a `sudo -n /usr/local/sbin/andler-helper <subcommand> ...` command. Used by `connect_nbd` (`nbd-connect`), the guards (`nbd-disconnect`, `umount`), `try_autoload_nbd_module` (`modprobe-nbd`), `mount_partition` (`mount-partition`), the chroot environment (`mount-bind`/`mount-tmpfs`/`guest-write`), and the ARM-translator file ops (`file`) — a single NOPASSWD rule authorizes exactly this one binary, and the helper re-validates every argument itself. Non-interactive: fails immediately instead of hanging on a password prompt. |
| `describe_helper_failure` | `(stderr: &str) -> String` | Rewrite stderr from a failed `sudo -n andler-helper` invocation into an actionable error message pointing at `andler doctor --fix` / the single sudoers rule (see `docs/DEVELOPMENT.md`, "Passwordless sudo for privileged operations"). |
| `wait_for_partitions` | `(nbd_dev: &Path) -> Result<Vec<PathBuf>, DiskError>` | Wait for partition devices to appear after NBD connect (polls `/sys/block/<dev>/` for up to 5s). |
| `find_root_partition` | `(partitions: &[PathBuf]) -> Result<PathBuf, DiskError>` | Identify the root partition as the **last** entry (partition ordering follows the partition table, so the last partition is the last logical one — not "largest by sector count"; on UKI images the ESP holds boot files, not the OS). |
| `unique_mount_name` | `() -> String` | Generate a unique mount point name under the runtime dir. |
| `mount_partition` | `(partition: &Path) -> Result<MountGuard, DiskError>` | Mount a partition (mode 0755 under `$XDG_RUNTIME_DIR`) and return `MountGuard` (RAII: unmounts + detaches on drop). After mounting, prepares the chroot with `bind_host_mounts`. |
| `bind_host_mounts` | `(mount_point: &Path)` | Make the chroot usable for real package-manager runs (called inside `mount_partition`): writes the host's nameservers into the guest `/etc/resolv.conf` (a regular file — a bind-mount fails with ENOENT when the guest file is a dangling `stub-resolv.conf` symlink), bind-mounts `/dev`, `/proc`, `/sys`, and mounts a fresh tmpfs on the guest `/run` (gpg-agent's sockets are otherwise unwritable on disk → pacman fails with "GPGME error: Invalid crypto engine"). Failures degrade to `tracing::warn!`. |
| `host_nameservers` | `() -> Option<String>` | Collect deduplicated `nameserver <ip>` lines from `/etc/resolv.conf`, falling back to `/run/systemd/resolve/stub-resolv.conf`. |
| `write_guest_resolv` | `(mount_point: &Path, contents: &str) -> io::Result<()>` | Replace the guest's `/etc/resolv.conf` through the `guest-write` helper subcommand (stdin pipe) — the guest fs is root-owned and the daemon runs unprivileged, so direct writes get EPERM. |

**`NbdGuard`**: RAII guard — holds the per-disk `flock` for its entire lifetime and disconnects the NBD device (`qemu-nbd --disconnect`) on drop; releasing the lock on drop is what lets another process's NBD operation on the same disk proceed.
**`MountGuard`**: RAII guard — unmounts and detaches when dropped.
**`NbdStatus`**: `loaded: bool`, `free_devices: usize`, `total_devices: usize`.

**Security:** `unique_mount_name()` generates mount points under `$XDG_RUNTIME_DIR` (typically `/run/user/{uid}/`), NOT `/tmp`. This avoids symlink attacks — `/tmp` is world-writable, allowing an unprivileged attacker to create a symlink to a sensitive host path (e.g., `/home/user`) and trick the daemon into mounting over it during NBD operations.

### `arm_translator` — ARM Translation Layer Management

Switches ARM translation libraries (libndk / libhoudini) in a mounted Android disk image.

| Function | Signature | Description |
|----------|-----------|-------------|
| `switch_translator` | `(overlay_path: &Path, translator: ArmTranslator, translator_dir: Option<PathBuf>, android_version: &str) -> Result<(), DiskError>` | Replace the current ARM translator in a mounted disk. No-op if the requested translator is already active. **Atomic**: new files are staged into `system/.andler-translator-staging` on the guest fs first (copy + rename), the old translator (`libndk_translation.so` / `libhoudini.so` and friends) is removed only after staging succeeds, then staged files are renamed into place and the staging dir removed. File lists support `lib/libndk*`-style wildcards, expanded against the actual archive/cache contents. Props: every key in `MANAGED_PROP_KEYS` is first removed from `build.prop`, then the translator's props are written (sorted); a missing `build.prop` (never-booted instance) is fine and starts from an empty set. `translator_dir` points at a local extracted cache (skips the download); otherwise the translator is obtained from the version-keyed cache or downloaded + MD5-verifi… |

### `translator` — Translator Metadata

Static metadata about each supported ARM translator (download links, files to install, properties to set).

| Function | Signature | Description |
|----------|-----------|-------------|
| `resolve` | `(translator: ArmTranslator) -> TranslatorInfo` | Look up download links (version-keyed: per Android version a `(version, url, md5)` triple), file lists, property overrides, and init.rc content for a translator variant. |
| `dir_name` | `(translator: ArmTranslator) -> &'static str` | Return the cache directory name for a translator (`"ndk"`, `"houdini"`, or `"none"`). |

**`TranslatorInfo`**: `dl_links`, `files`, `props`, `init_rc`, `detect_file`.

**`MANAGED_PROP_KEYS`**: every `build.prop` key the translators own — used by `switch_translator` to strip stale keys (including `ro.vendor.enable.native.bridge.exec[64]` and `ro.ndk_translation.version`) before writing the new set, so switching `libndk` → `libhoudini` → `None` never leaks props.

**`houdini::INIT_RC`**: a functional replacement for the archive's init script (byte-identical to the waydroid-helper reference): mounts `binfmt_misc` on `early-init` and registers the `arm_{exe,dyn}` / `arm64_{exe,dyn}` handlers via `echo … > /proc/sys/fs/binfmt_misc/register` once `ro.enable.native.bridge.exec=1` is set.

### `translator_download` — Translator Download & Cache

Downloads and caches ARM translator archives (ZIP files) under `~/.andler/cache/arm-translators/` (see `andler_core::paths::arm_translators_dir`).

| Function | Signature | Description |
|----------|-----------|-------------|
| `ensure_translator` | `(translator: ArmTranslator, android_version: &str) -> Result<PathBuf, DiskError>` | Ensure the translator is downloaded and extracted. Returns the cache path (`<arm-translators>/<dir_name>`). Skips the download if the detect file already exists in cache; otherwise picks the URL/MD5 for the requested `android_version` from `TranslatorInfo::dl_links` (error if no link matches), downloads, verifies the MD5 (`BadChecksum` path), and unzips. Extraction flattens the GitHub `<repo>-<commit>/prebuilts/` archive wrapper, so `bin/`/`etc/`/`lib/`/`lib64/` land directly under the cache root (this is also what makes the detect-file cache hit work). |

### ARM Translation — Design Notes, Fix History & Reference Comparison

How `guest install libndk`/`libhoudini` works, what was broken before, and
how the reference implementations do it. Last cross-verified against
waydroid-helper and waydroid_script on 2026-08-07.

#### Mechanism

1. Resolve the translator's `FILES` / `PROPS` / `INIT_RC` / `DETECT_FILE`
   (`translator/{ndk,houdini}.rs`).
2. Without `--translator-dir`, `ensure_translator()` downloads the
   version-keyed archive (`11`/`13`), verifies its MD5, and extracts it
   into `~/.andler/cache/arm-translators/<dir_name>/`; extraction flattens
   the `<repo>-<commit>/prebuilts/` wrapper so the payload sits directly
   under the cache root.
3. The instance disk is attached via `qemu-nbd`, the root partition is
   mounted, and the target becomes `/var/lib/waydroid/overlay/system/`
   (created if missing — waydroid merges this overlay over `/system` at
   container start, so a never-booted instance is supported).
4. If the target's `DETECT_FILE` already exists, the install is a no-op
   ("already installed, skipping").
5. **Stage** — every `FILES` entry (wildcards like `lib/libndk*` are
   expanded against the source dir) is copied into
   `system/.andler-translator-staging/`; `bin`-component paths get 0755.
   Missing concrete entries log a warning and are skipped (the built-in
   `houdini.rc` covers houdini's missing archive rc).
6. **Remove** the previous translator's files (same expansion, against the
   system dir) and its `<dir_name>.rc`.
7. **Move** staged files into place, drop the staging dir.
8. **Props** — the merge always starts from `base_build_prop()`: the guest's
   pristine build.prop *below* the overlay — a plain `system/build.prop`, or
   (waydroid mainline layout) `/system/build.prop` extracted with `debugfs`
   from `etc/waydroid-extra/images/system.img` (e2fsprogs, essential; no root
   to read the image). The existing upper file is **never** a source — it is
   derived data and reinstalling regenerates it, so a partial upper written
   by an older buggy build is repaired instead of perpetuated. Without any
   base source the install fails loudly (an upper with only translator props
   would shadow the base's `ro.*` wholesale and break Android boot). Every
   key in `MANAGED_PROP_KEYS` is removed, then the translator's `PROPS` are
   written (sorted).
9. **Init script** — if `INIT_RC` is `Some`, it overwrites
   `etc/init/<dir_name>.rc` (parent dir created).

#### Properties

`MANAGED_PROP_KEYS` is the union of every key any translator owns (incl.
`None`), so switching libndk → libhoudini → None can never leak stale
keys (`ro.ndk_translation.version`, `ro.vendor.enable.native.bridge.exec[64]`,
`ro.dalvik.vm.native.bridge`, the abilist trio, `ro.dalvik.vm.isa.*`,
`ro.enable.native.bridge.exec`).

`ro.product.cpu.abilist` uses the order `x86_64,arm64-v8a,x86,armeabi-v7a,armeabi`
— the same order the official Google images ship (ChromeOS zork
ndk_translation and WSA houdini `prop.json`). waydroid_script hardcodes the
legacy `x86_64,x86,arm64-v8a,…` order; the difference only affects apps that
ship both x86 and arm64 variants (which ABI wins the ranking).

#### Init scripts

- **houdini** — `INIT_RC` is a functional rc, byte-identical to the
  waydroid-helper reference `houdini.rc` (pinned by the test
  `houdini_init_rc_matches_reference_binfmt_registration` against
  `tests/fixtures/houdini.rc`): it mounts `binfmt_misc` on `early-init`
  and, once `ro.enable.native.bridge.exec=1` is set, registers the four
  handlers (`arm_{exe,dyn}`, `arm64_{exe,dyn}`) by `exec`ing
  `/system/bin/sh -c "echo ':arm_exe:M::\\\\x7f…' >> register"`. The
  `\\x7f` ELF-magic escapes are written with **four** backslashes because
  they are unescaped twice before the kernel sees them: AOSP init's
  tokenizer halves them (`\\`→`\`, verified against android11/13
  `init/tokenizer.cpp`), the single-quoted shell keeps them verbatim, and
  mksh's `echo` (escape processing on by default) halves them again, so
  the register write carries canonical `\x7f`, which binfmt decodes to
  the ELF magic bytes.
- **ndk** — the archive's own `etc/init/ndk_translation.rc` is kept; it
  `copy`s its rules into `binfmt_misc` itself.

#### Pins (all MD5-verified against the downloaded archives)

| Translator | Android | Commit | MD5 |
|---|---|---|---|
| NDK (`libndk`) | 11 | `9324a8914b649b885dad6f2bfd14a67e5d1520bf` | `c9572672d1045594448068079b34c350` |
| NDK (`libndk`) | 13 | `68734c52556d3d7a6db34c603dd9276915c29f2f` | `0b2207c490fcb400aa5c87fcf0d52d38` |
| Houdini (`libhoudini`) | 11 | `cf7f970f6004f0c329b0464e3d65f9b0e2baea91` | `5554b11cba905058c3d9bb5e45535d83` |
| Houdini (`libhoudini`) | 13 | `debc3dc91cf12b5c5b8a1c546a5b0b7bf7f838a8` | `cb7ffac26d47ec7c89df43818e126b47` |

NDK pins are identical to waydroid-helper's `ndk_translation-chromeos_zork-R136`
package. The houdini pins are the waydroid-helper wsa11 (11) and hpe-14 (13)
commits; their payloads are **byte-identical** (per-file MD5 diff) to the
archives waydroid_script pins (`81f2a51e…` for 11, `9e778963…` for 13) — the
only difference is that those ship a `etc/init/houdini.rc` stub, which ANDLER
overwrites with its own functional rc anyway.

#### Fixed defects (audit 2026-08-07, all fixed in the same pass)

The install flow was live-audited against real archives and a sandbox
instance before the fixes; the defects found and how they were fixed:

| # | Defect | Fix |
|---|---|---|
| 1 | NDK payload incomplete: `etc/binfmt_misc/*`, `etc/cpuinfo.arm{.64}.txt`, `etc/ld.config.arm{.64}.txt`, `bin/ndk_translation_program_runner_binfmt_misc_arm64` were missing from `FILES` — the archive rc `copy`s its rules from `etc/binfmt_misc`, so registration failed | entries added to `ndk.rs::FILES` |
| 2 | `lib/libndk*` / `lib64/libndk*` wildcards never matched (literal `exists()` check) — the native-bridge library, all proxy shims, the detect file were never installed; NDK installs were never idempotent | wildcard expansion in `resolve_entry_paths()` for stage/move/removal |
| 3 | Online path broken: the zip's `<repo>-<commit>/prebuilts/` prefix broke both the cache-hit check and the install joins — default `guest install` silently copied nothing and re-downloaded every run | `flatten_prebuilts()` in `extract_zip` |
| 4 | Houdini rc was an empty stub (trigger lines, no bodies) that overwrote the archive's working rc — binfmt rules never registered | functional rc (waydroid-helper reference, byte-identical) |
| 5 | Houdini pins matched neither reference ecosystem | rebased on waydroid-helper wsa11/hpe-14 commits (payload-identical to waydroid_script's pins) |
| 6 | Old payloads survived a switch (archive `ndk_translation.rc`, `bin/arm`, `bin/arm64` leftovers) | removal now uses the same expansion; stale rc (`<dir_name>.rc`) is always removed |
| 7 | Props leaked between translators (`ro.ndk_translation.version`, `ro.vendor.*` stayed after switching to houdini) | `MANAGED_PROP_KEYS` remove-then-set on every switch, incl. `None` |
| 8 | Fresh install hard-errored when `build.prop` was absent (never-booted instance) | the upper `build.prop` is always regenerated from the base below the overlay: plain `system/build.prop`, or `/system/build.prop` extracted with `debugfs` from `etc/waydroid-extra/images/system.img`; no base source is a hard error, never a silent partial upper |
| 9 | Privilege surface: the install needed NOPASSWD `cp/mkdir/mv/rm/chmod` beyond the documented NBD set | all guest-fs mutations go through the `file`/`guest-write` subcommands of the single privileged **`andler-helper`** binary (sole NOPASSWD rule, `apps/helper`); `andler doctor --fix` installs the binary and migrates the old per-binary rules away |
| 10 | **Live boot failure: Android stuck at boot after a translator install.** The upper overlay `build.prop` contained only the 10 translator props and shadowed the base's full build.prop wholesale — waydroid could not parse the Android version from the merged rootfs (`Failed to parse android version from system.img: invalid literal for int() with base 10: ''`) and ART's `derive_classpath` aborted, so the container never finished booting | `base_build_prop()` extracts the base's `/system/build.prop` from `system.img` via `debugfs` and merges translator props into the full file; the upper is regenerated on every install (never read back), so already-broken installs are repaired by re-running `guest install` (or `config set arm_translator none`); E2E 07 crafts a `system.img` fixture and asserts the merged upper; regression tests `base_build_prop_reads_plain_layout_and_errors_without_source`, `base_build_prop_prefers_plain_layout_over_system_image`, `parse_build_prop_skips_blank_and_comment_lines` |

#### Reference comparison (waydroid-helper / waydroid_script)

- **waydroid-helper** installs by copying the entire `prebuilts/*` tree into
  `$pkgdir/system` (an Arch `package()`), applies props through waydroid's
  property layer, and overwrites `houdini.rc` with the functional rc. ANDLER
  does the same with an explicit file list (equivalent — the list mirrors the
  archive payload; on the Android 11 pin it also keeps `bin/arm/linker` and
  `bin/arm64/linker64`, while the Android 13 pin lacks `bin/arm`/`bin/arm64`
  entirely and the expander skips the missing entries with a warning),
  writes props into the overlay `build.prop` directly
  (no guest-side tooling needed), and ships the same rc.
- **waydroid_script** (casualsnek) copies the whole tree, writes props into
  `waydroid.cfg` (applied by a `resetprop` helper at boot), and embeds the
  same rc (modulo a cosmetic leading newline). Its prop key sets are exactly
  ANDLER's; its uninstall does **not** clean props (ANDLER's managed-key
  removal is stricter). Its abilist order is the legacy one (see Properties).
- Both references' pins and ANDLER's are mutually consistent, as verified by
  MD5 and per-file diffs of the actual archives (2026-08-07).

#### Reproducing the live verification

Sandbox recipe used for the audit (all temp, removed afterwards): build a
2 GiB qcow2 with an ext4 root partition, run `andlerd` on a scratch port with
`ANDLER_HOME` pointed at a temp dir (as root, via the single
`/usr/local/sbin/andler-helper` NOPASSWD rule that `andler doctor --fix`
installs — or sidestepping the helper entirely by running as root),
create an Android 13 instance with
that disk as base, then `andler guest install libndk`/`libhoudini` and mount
the overlay to inspect `bin/`, `etc/`, `lib/`, `lib64/`, `build.prop` and
`etc/init/houdini.rc` (md5-compare the rc against `tests/fixtures/houdini.rc`).

## Error Types

**`DiskError`**:

| Variant | Fields | Description |
|---------|--------|-------------|
| `SpawnFailed` | `io::Error` | `qemu-img` binary not found or no exec permissions |
| `CommandFailed` | `status: i32`, `stderr: String` | `qemu-img` exited non-zero |
| `BackingFileNotFound` | `PathBuf` | Backing file doesn't exist (checked before spawning) |
| `ParseError` | `String` | Failed to parse `qemu-img info --output=json` |
| `Io` | `path`, `source` | Filesystem error at path |
| `FileSystem` | `String` | Generic filesystem operation failure |
| `NbdSetupFailed` | `String` | NBD device error (module not loaded, no free device, mount/umount failure) |
| `ShrinkRequiresConfirmation` | `path`, `current_size_bytes`, `requested_size_bytes` | Refusing to shrink without `--shrink` flag |
| `CompactNotApplicable` | `path`, `format` | Compact only works on qcow2 disks |
| `PackageManagerNotFound` | `mount_point: PathBuf` | No known package manager binary in guest filesystem |
| `AgentAlreadyInstalled` | `package: String` | Package already installed in guest |
| `AgentNotInstalled` | `package: String` | Package not found in guest for removal |
| `GuestAgentUnavailable` | `instance_id: String` | Guest agent (qemu-ga) not available for online operations |
| `InsufficientDiskSpace` | `path`, `required_bytes`, `available_bytes` | Not enough free space on `path`'s filesystem for the operation (pre-checked, not a failure mid-operation) |

## Tests

### Without `qemu-img` / `/dev/kvm`

- **`qcow2`**: JSON field parsing (`parse_json_u64_field` — compact/spaced/missing/zero/nested-field-ignored; `parse_json_string_field` — compact/spaced/missing/empty; `parse_json_optional_string_field` — value/missing/empty).
- **`clone`**: `shared_base_clone_reports_missing_source_as_io_error`.
- **`boot_mode`**: `read_boot_mode_recognizes_android_target`, `read_boot_mode_treats_anything_else_as_linux`, `read_boot_mode_errors_when_no_default_target_link`.
- **`diskspace`**: `available_bytes_on_temp_dir_is_nonzero`, `available_bytes_resolves_to_nearest_existing_ancestor`, `check_available_space_passes_for_a_tiny_requirement`, `check_available_space_fails_for_an_absurd_requirement`.
- **`guest_tools`**: `detect_package_manager_returns_apt`/`dnf`/`pacman`/`none_for_empty_dir`, `package_manager_install_args`, `package_manager_remove_args`, `check_package_status_offline_returns_installed_*`/`not_installed_*`, `known_packages_has_entries`.
- **`nbd`**: `lock_path_for_is_colocated_and_hidden`, `acquire_disk_lock_succeeds_on_a_fresh_path`, `acquire_disk_lock_is_reentrant_within_the_same_process`, `find_free_nbd_device_returns_existing_device_or_explains_absence` (environment-tolerant), `unique_mount_name_is_unique`, `find_root_partition_picks_the_last_partition_not_the_esp`, `find_root_partition_errors_on_empty_list`.
- **`arm_translator`**: `detect_current_translator_returns_none_on_empty_dir`, `build_prop_content_sorts_keys_and_appends_newline`, `resolve_entry_paths_expands_wildcards_in_parent_dir`, `resolve_entry_paths_returns_empty_for_unmatched_wildcard`.
- **`translator_download`**: `extract_zip_flattens_repo_prebuilts_prefix`, `flatten_prebuilts_does_not_touch_already_flat_payload_dirs`, `flatten_prebuilts_errors_on_collision_instead_of_silently_skipping` (plus offline download/checksum error paths).
- **`arm_translator`**: `detect_current_translator_returns_none_on_empty_dir`, `build_prop_content_sorts_keys_and_appends_newline`, `resolve_entry_paths_expands_wildcards_in_parent_dir`, `resolve_entry_paths_slash_star_matches_whole_directory`, `resolve_entry_paths_returns_empty_for_unmatched_wildcard`, `base_build_prop_reads_plain_layout_and_errors_without_source`, `base_build_prop_prefers_plain_layout_over_system_image`, `parse_build_prop_skips_blank_and_comment_lines`.
- **`translator`**: `resolve_ndk_has_links`, `resolve_houdini_has_links`, `resolve_none_has_no_links`, `dir_names` (asserts `"ndk"`/`"houdini"`/`"none"`), `managed_prop_keys_cover_every_translator_key`, `houdini_init_rc_matches_reference_binfmt_registration` (byte-identical to `tests/fixtures/houdini.rc`, the waydroid-helper reference).

### With `qemu-img` (integration tests, `#[ignore]`)

- **`qcow2`**: `create_then_virtual_size_round_trips`, `create_with_backing_file_fails_fast_on_missing_backing`, `resize_grow_succeeds_without_confirmation`, `resize_shrink_without_confirmation_is_rejected`, `resize_shrink_with_confirmation_succeeds`, `compact_qcow2_succeeds`, `compact_raw_is_rejected_as_not_applicable`.
- **`clone`**: `linked_clone_points_at_source_instance_disk`, `full_standalone_clone_has_no_backing_file`, `shared_base_clone_does_not_depend_on_source_after_copy` (explicitly deletes source after copy, verifies clone survives).
- **`overlay`**: `create_overlay_points_at_given_base_image`, `create_overlay_fails_when_base_image_missing`, `factory_reset_recreates_overlay`.

All marked `#[ignore]` — run in `integration-test` Docker target.

## What Is NOT Here

- Network configuration — that's `andler-net`.
- Instance lifecycle management — that's `andler-daemon`.
