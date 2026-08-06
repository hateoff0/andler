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
| `is_agent_installed` | `(mount_point: &Path, pm: PackageManager, package: &str) -> Result<bool, DiskError>` | Check installation via the manager's query (`dpkg -l` / `rpm -q` / `pacman -Q`), run inside the chroot through `sudo -n` |
| `install_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Install package offline (NBD connect → mount → index refresh → chroot install). Wrapped in `spawn_blocking`. |
| `remove_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Remove package offline (NBD connect → mount → chroot remove). Wrapped in `spawn_blocking`. |
| `check_package_status_offline` | `(mount_point: &Path, binary_checks: &[&str]) -> PackageStatus` | Check binary presence in mounted filesystem — any candidate path (`/usr/bin/...` and `/usr/sbin/...`) marks the package installed |
| `check_all_packages_offline` | `(mount_point: &Path) -> Vec<(&GuestPackage, PackageStatus)>` | Check all KNOWN_PACKAGES in mounted filesystem |
| `check_all_packages_offline_with_disk` | `(disk_path: &Path) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | Full offline check: NBD connect + mount + check + unmount |
| `check_android_packages_offline_with_disk` | `(disk_path: &Path) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | Full offline check for Android packages: NBD connect + mount + check + unmount |
| `available_packages` | `(kind: &InstanceKind) -> &'static [GuestPackage]` | Return package list for instance kind: `ANDROID_PACKAGES` for Android, `KNOWN_PACKAGES` for Linux |

**`PackageManager`**: `Apt` | `Dnf` | `Pacman`, with `binary_name()`, `install_args(pkg)`, `remove_args(pkg)`, `check_installed_command(pkg)` helpers.

**`PackageStatus`**: `Installed` | `NotInstalled` | `Unknown` (the check scripts can exit non-zero for reasons other than "not installed").

**Offline install flow**: connect NBD → wait for partitions → mount root partition → detect package manager → refuse if already installed (`AgentAlreadyInstalled`) → refresh indexes first (`apt-get update` / `dnf makecache` / `pacman -Sy`) so installs succeed on fresh images → chroot install. An index-refresh failure is a hard error (reported with exit status + stderr via `describe_sudo_failure`), and the guest's `/etc/resolv.conf` content (or its absence / dangling-symlink metadata) is logged at `info` for DNS diagnosis. All privileged steps run via NOPASSWD `sudo -n chroot <mount> ...`.

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
| `privileged_command` | `(program: &str) -> Command` | Build a `sudo -n <program> ...` command. Used by `connect_nbd`, `umount`, `chroot`, and `mount` operations that need root (opening `/dev/nbd*`, lock files in `/var/lock`, guest-fs mutations). Non-interactive: fails immediately instead of hanging on a password prompt. |
| `describe_sudo_failure` | `(program: &str, stderr: &str) -> String` | Rewrite stderr from a failed `sudo -n` into an actionable error message naming the missing sudoers rule (see `docs/DEVELOPMENT.md`, "Passwordless sudo for privileged operations"). |
| `wait_for_partitions` | `(nbd_dev: &Path) -> Result<Vec<PathBuf>, DiskError>` | Wait for partition devices to appear after NBD connect (polls `/sys/block/<dev>/` for up to 5s). |
| `find_root_partition` | `(partitions: &[PathBuf]) -> Result<PathBuf, DiskError>` | Identify the root partition as the **last** entry (partition ordering follows the partition table, so the last partition is the last logical one — not "largest by sector count"; on UKI images the ESP holds boot files, not the OS). |
| `unique_mount_name` | `() -> String` | Generate a unique mount point name under the runtime dir. |
| `mount_partition` | `(partition: &Path) -> Result<MountGuard, DiskError>` | Mount a partition (mode 0755 under `$XDG_RUNTIME_DIR`) and return `MountGuard` (RAII: unmounts + detaches on drop). After mounting, prepares the chroot with `bind_host_mounts`. |
| `bind_host_mounts` | `(mount_point: &Path)` | Make the chroot usable for real package-manager runs (called inside `mount_partition`): writes the host's nameservers into the guest `/etc/resolv.conf` (a regular file — a bind-mount fails with ENOENT when the guest file is a dangling `stub-resolv.conf` symlink), bind-mounts `/dev`, `/proc`, `/sys`, and mounts a fresh tmpfs on the guest `/run` (gpg-agent's sockets are otherwise unwritable on disk → pacman fails with "GPGME error: Invalid crypto engine"). Failures degrade to `tracing::warn!`. |
| `host_nameservers` | `() -> Option<String>` | Collect deduplicated `nameserver <ip>` lines from `/etc/resolv.conf`, falling back to `/run/systemd/resolve/stub-resolv.conf`. |
| `write_guest_resolv` | `(mount_point: &Path, contents: &str) -> io::Result<()>` | Replace the guest's `/etc/resolv.conf` through NOPASSWD `sudo chroot ... /bin/sh -c 'rm -f /etc/resolv.conf && cat > /etc/resolv.conf'` (stdin pipe) — the guest fs is root-owned and the daemon runs unprivileged, so direct writes get EPERM. |

**`NbdGuard`**: RAII guard — holds the per-disk `flock` for its entire lifetime and disconnects the NBD device (`qemu-nbd --disconnect`) on drop; releasing the lock on drop is what lets another process's NBD operation on the same disk proceed.
**`MountGuard`**: RAII guard — unmounts and detaches when dropped.
**`NbdStatus`**: `loaded: bool`, `free_devices: usize`, `total_devices: usize`.

**Security:** `unique_mount_name()` generates mount points under `$XDG_RUNTIME_DIR` (typically `/run/user/{uid}/`), NOT `/tmp`. This avoids symlink attacks — `/tmp` is world-writable, allowing an unprivileged attacker to create a symlink to a sensitive host path (e.g., `/home/user`) and trick the daemon into mounting over it during NBD operations.

### `arm_translator` — ARM Translation Layer Management

Switches ARM translation libraries (libndk / libhoudini) in a mounted Android disk image.

| Function | Signature | Description |
|----------|-----------|-------------|
| `switch_translator` | `(overlay_path: &Path, translator: ArmTranslator, translator_dir: Option<PathBuf>, android_version: &str) -> Result<(), DiskError>` | Replace the current ARM translator in a mounted disk. No-op if the requested translator is already active. **Atomic**: new files are staged into `system/.andler-translator-staging` on the guest fs first (copy + rename, `build.prop` merged and written sorted), the old translator (`libndk_translation.so` / `libhoudini.so` and friends) is removed only after staging succeeds, then staged files are renamed into place and the staging dir removed. `translator_dir` points at a local extracted cache (skips the download); otherwise the translator is obtained from the version-keyed cache or downloaded + MD5-verified + ZIP-extracted via `ensure_translator`. |

### `translator` — Translator Metadata

Static metadata about each supported ARM translator (download links, files to install, properties to set).

| Function | Signature | Description |
|----------|-----------|-------------|
| `resolve` | `(translator: ArmTranslator) -> TranslatorInfo` | Look up download links (version-keyed: per Android version a `(version, url, md5)` triple), file lists, property overrides, and init.rc content for a translator variant. |
| `dir_name` | `(translator: ArmTranslator) -> &'static str` | Return the cache directory name for a translator (`"ndk"`, `"houdini"`, or `"none"`). |

**`TranslatorInfo`**: `dl_links`, `files`, `props`, `init_rc`, `detect_file`.

### `translator_download` — Translator Download & Cache

Downloads and caches ARM translator archives (ZIP files) under `~/.andler/cache/arm-translators/` (see `andler_core::paths::arm_translators_dir`).

| Function | Signature | Description |
|----------|-----------|-------------|
| `ensure_translator` | `(translator: ArmTranslator, android_version: &str) -> Result<PathBuf, DiskError>` | Ensure the translator is downloaded and extracted. Returns the cache path (`<arm-translators>/<dir_name>`). Skips the download if the detect file already exists in cache; otherwise picks the URL/MD5 for the requested `android_version` from `TranslatorInfo::dl_links` (error if no link matches), downloads, verifies the MD5 (`BadChecksum` path), and unzips. |

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
- **`arm_translator`**: `detect_current_translator_returns_none_on_empty_dir`.
- **`translator`**: `resolve_ndk_has_links`, `resolve_houdini_has_links`, `resolve_none_has_no_links`, `dir_names` (asserts `"ndk"`/`"houdini"`/`"none"`).

### With `qemu-img` (integration tests, `#[ignore]`)

- **`qcow2`**: `create_then_virtual_size_round_trips`, `create_with_backing_file_fails_fast_on_missing_backing`, `resize_grow_succeeds_without_confirmation`, `resize_shrink_without_confirmation_is_rejected`, `resize_shrink_with_confirmation_succeeds`, `compact_qcow2_succeeds`, `compact_raw_is_rejected_as_not_applicable`.
- **`clone`**: `linked_clone_points_at_source_instance_disk`, `full_standalone_clone_has_no_backing_file`, `shared_base_clone_does_not_depend_on_source_after_copy` (explicitly deletes source after copy, verifies clone survives).
- **`overlay`**: `create_overlay_points_at_given_base_image`, `create_overlay_fails_when_base_image_missing`, `factory_reset_recreates_overlay`.

All marked `#[ignore]` — run in `integration-test` Docker target.

## What Is NOT Here

- Network configuration — that's `andler-net`.
- Instance lifecycle management — that's `andler-daemon`.
