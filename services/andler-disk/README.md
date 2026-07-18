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
| `resize` | `(path: &Path, new_size_bytes: u64, allow_shrink: bool) -> Result<(), DiskError>` | Change logical size. Corresponds to `qemu-img resize`. Shrinking (`new_size_bytes` < current) without `allow_shrink = true` returns `DiskError::ShrinkRequiresConfirmation` instead of touching the file — see PLAN.md, "Disk management". |
| `compact` | `(path: &Path) -> Result<(), DiskError>` | Remove free blocks via `qemu-img convert` to temp file + atomic rename. qcow2-only — returns `DiskError::CompactNotApplicable` for other formats (e.g. raw has no reclaimable metadata). |
| `virtual_size_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read logical (virtual) size from `qemu-img info --output=json`. |
| `disk_usage_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read actual disk usage from `qemu-img info --output=json` (`actual-size` field). |

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

### `guest_tools` — Offline Guest Package Management

Checks and manages packages in guest OS filesystems via `qemu-nbd` + mount + chroot.

| Function | Signature | Description |
|----------|-----------|-------------|
| `detect_package_manager` | `(mount_point: &Path) -> Option<PackageManager>` | Detect package manager from binary presence (`apt-get`/`dnf`/`pacman`) |
| `is_agent_installed` | `(mount_point: &Path, pm: PackageManager, package: &str) -> bool` | Check if a package is installed via its binary |
| `install_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Install package offline (mount + chroot + pkg install) |
| `remove_agent_offline` | `(disk_path: &Path, package: &str) -> Result<(), DiskError>` | Remove package offline (mount + chroot + pkg remove) |
| `check_package_status_offline` | `(mount_point: &Path, binary_check: &str) -> PackageStatus` | Check binary presence in mounted filesystem |
| `check_all_packages_offline` | `(mount_point: &Path) -> Vec<(&GuestPackage, PackageStatus)>` | Check all KNOWN_PACKAGES in mounted filesystem |
| `check_all_packages_offline_with_disk` | `(disk_path: &Path) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | Full offline check: NBD connect + mount + check + unmount |

**Known Packages** (`KNOWN_PACKAGES`): `spice-vdagent` (`/usr/bin/spice-vdagentd`), `qemu-guest-agent` (`/usr/bin/qemu-ga`), `spice-webdavd` (`/usr/bin/spice-webdavd`).

### `diskspace` — Free Disk Space Pre-check

Checks free space on a path's filesystem via `statvfs(2)` before an operation that could otherwise fail partway through with a raw ENOSPC — see ROADMAP.md, "Core: add disk space pre-check before snapshot operations".

| Function | Signature | Description |
|----------|-----------|--------------|
| `check_available_space` | `(path: &Path, required_bytes: u64) -> Result<(), DiskError>` | Fails with `InsufficientDiskSpace` if fewer than `required_bytes` are free on `path`'s filesystem (resolved to its nearest existing ancestor if `path` doesn't exist yet) |

Uses `f_bavail` (blocks available to an unprivileged user), not `f_bfree` (which includes root-reserved blocks the daemon may not actually be able to use, e.g. ext4's default 5% reservation).

## Error Types

**`DiskError`**:

| Variant | Fields | Description |
|---------|--------|-------------|
| `SpawnFailed` | `io::Error` | `qemu-img` binary not found or no exec permissions |
| `CommandFailed` | `status: i32`, `stderr: String` | `qemu-img` exited non-zero |
| `BackingFileNotFound` | `PathBuf` | Backing file doesn't exist (checked before spawning) |
| `ParseError` | `String` | Failed to parse `qemu-img info --output=json` |
| `Io` | `path`, `source` | Filesystem error at path |
| `NbdSetupFailed` | `String` | NBD device error (module not loaded, no free device, mount/umount failure) |
| `ShrinkRequiresConfirmation` | `path`, `current_size_bytes`, `requested_size_bytes` | Refusing to shrink without `--shrink` flag |
| `CompactNotApplicable` | `path`, `format` | Compact only works on qcow2 disks |
| `PackageManagerNotFound` | `mount_point: PathBuf` | No known package manager binary in guest filesystem |
| `AgentAlreadyInstalled` | `package: String` | Package already installed in guest |
| `AgentNotInstalled` | `package: String` | Package not found in guest for removal |
| `GuestAgentUnavailable` | `package: String` | Guest agent (qemu-ga) not available for online operations |
| `InsufficientDiskSpace` | `path`, `required_bytes`, `available_bytes` | Not enough free space on `path`'s filesystem for the operation (pre-checked, not a failure mid-operation) |

## Tests

### Without `qemu-img` / `/dev/kvm`

- **`qcow2`** (21 tests): JSON field parsing from `qemu-img info` output, create/resize/compact operations, `ShrinkRequiresConfirmation` error path.
- **`clone`** (4 tests): `shared_base_clone_reports_missing_source_as_io_error`, `linked_clone_points_at_source`, `full_standalone_clone_has_no_backing`, `shared_base_clone_survives_source_deletion`.
- **`overlay`** (3 tests): `create_overlay_points_at_base_image`, `create_overlay_fails_when_base_image_missing`, `factory_reset_recreates_overlay`.
- **`guest_tools`** (3 tests): `detect_package_manager_*` (2 tests), `package_manager_install_args` (1 test), `check_package_status_offline_*` (2 tests), `known_packages_has_entries` (1 test).

### With `qemu-img` (integration tests, `#[ignore]`)

- `create_then_virtual_size_round_trips`
- `create_with_backing_file_fails_fast_on_missing_backing`
- `linked_clone_points_at_source_instance_disk`
- `full_standalone_clone_has_no_backing_file`
- `shared_base_clone_does_not_depend_on_source_after_copy` (explicitly deletes source after copy, verifies clone survives)
- `create_overlay_points_at_given_base_image`
- `create_overlay_fails_when_base_image_missing`
- `factory_reset_recreates_overlay`

All marked `#[ignore]` — run in `integration-test` Docker target.

## What Is NOT Here

- Network configuration — that's `andler-net`.
- Instance lifecycle management — that's `andler-daemon`.
