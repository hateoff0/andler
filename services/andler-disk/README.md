# andler-disk

Disk operations for virtual machines: creation, cloning, resizing, compaction — a wrapper around `qemu-img`, plus domain-specific overlay disk logic for Android instances, and offline Magisk provisioning via `qemu-nbd`.

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

### `magisk` — Offline Magisk Provisioning

Installs Magisk root access into an Android overlay disk offline using `qemu-nbd` for NBD-based disk access. No running QEMU required.

**Public API**:

| Function | Signature | Description |
|----------|-----------|-------------|
| `provision_magisk` | `(overlay_path: &Path, magisk_dir: &Path) -> Result<(), DiskError>` | Full offline provisioning pipeline |

**Pipeline**:
1. Validate magisk directory (must contain `magisk` and `magiskinit` binaries)
2. Find free NBD device (`/sys/class/block/nbd*/size` == 0)
3. Connect overlay via `qemu-nbd --connect`
4. Wait for partition devices to appear (2s timeout)
5. Find root partition (first partition)
6. Mount partition rw
7. Copy Magisk files to `/data/adb/magisk/`
8. Create modules directory structure
9. Patch boot image via `magiskboot` (best-effort, skips if tool missing)
10. Unmount + disconnect (RAII cleanup on all error paths)

**RAII Guards** (private):
- **`NbdGuard`**: Runs `qemu-nbd --disconnect` on drop. Ensures NBD device is released even on panic.
- **`MountGuard`**: Runs `umount -l` + removes mount point on drop. Ensures clean unmount.

**Requires**: `nbd` kernel module loaded (`sudo modprobe nbd`), `qemu-nbd` binary.

## Error Types

**`DiskError`**:

| Variant | Fields | Description |
|---------|--------|-------------|
| `SpawnFailed` | `io::Error` | `qemu-img` binary not found or no exec permissions |
| `CommandFailed` | `status: i32`, `stderr: String` | `qemu-img` exited non-zero |
| `BackingFileNotFound` | `PathBuf` | Backing file doesn't exist (checked before spawning) |
| `ParseError` | `String` | Failed to parse `qemu-img info --output=json` |
| `Io` | `path`, `source` | Filesystem error at path |
| `MagiskDirInvalid` | `String` | Magisk directory invalid or missing required files |
| `NbdSetupFailed` | `String` | NBD device error (module not loaded, no free device, mount/umount failure) |

## Tests

### Without `qemu-img` / `/dev/kvm`

- **`qcow2`** (3 tests): JSON field parsing from `qemu-img info` output.
- **`clone`** (1 test): `shared_base_clone_reports_missing_source_as_io_error` — `tokio::fs::copy` returns `ENOENT` before any external process.
- **`magisk`** (6 tests): `validate_magisk_dir_*` (3 tests), `find_free_nbd_device_*` (1 test), `copy_dir_recursive_*` (1 test), `unique_mount_name_*` (1 test).

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

- Provisioning root/Magisk **before first guest boot** (online provisioning via guest agent) — this is offline-only. `create_overlay` creates an overlay ready only for `RootMode::None` without calling `provision_magisk`.
- Network configuration — that's `andler-net`.
- Instance lifecycle management — that's `andler-daemon`.
