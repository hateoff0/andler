# Changelog

All notable changes to ANDLER will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

#### Core (`andler-core`)

- **Unified `create` command**: Single `create` command with `--kind linux`/`--kind android` flag to select VM type. TOML mode auto-detects type from content.
- **`--kind` flag**: `--kind linux` creates LinuxVm via CLI flags (`--iso-path`, `--disk-path`, `--ovmf-vars-template`). `--kind android` creates AndroidVm via CLI flags. Mutually exclusive with `--file`.
- **AndroidVm from TOML**: `InstanceFile` supports `android_version`, `base_image_path`, `overlay_size_gib`, `root`, `magisk_dir`, `gapps`, `microg`, `libndk`, `instances_root`. Auto-detected: presence of `android_version` or `base_image_path` → AndroidVm; otherwise LinuxVm.
- **Snapshot timeout**: Configurable per-instance (`DiskConfig::snapshot_timeout_secs`, default 30s) and per-operation (`--timeout` flag on `create`/`restore`/`delete`).
- **LinuxVm clone/export**: `CloneMode::Linked` and `CloneMode::FullStandalone` supported. `SharedBase` rejected with `SharedBaseNotSupportedForLinuxVm`.

#### Backend (`andler-qemu`)

- **QEMU VM snapshots**: Full CRUD — create, restore, delete, list snapshots via QEMU's `snapshot-save`/`snapshot-load`/`snapshot-delete` job API. Polls `query-jobs` for completion with configurable timeout.
- **Resource metrics from `/proc`**: Real-time streaming of CPU%, RAM usage, disk I/O, and network I/O. No QMP required. 1-second polling interval.
- **GPU metrics (AMD)**: Sysfs-based GPU metrics — VRAM used/total and GPU load percentage from `/sys/class/drm/card*/device/`.
- **GPU metrics (NVIDIA)**: `nvidia-smi` CLI-based GPU metrics — VRAM used/total (MiB) and GPU load %. Automatic vendor detection with AMD→NVIDIA→Intel priority.
- **GPU metrics (Intel)**: i915 sysfs-based GPU metrics — GPU load % via busyiffies delta, VRAM via stolen memory (approximate).
- **GPU vendor detection**: Automatic AMD → NVIDIA → Intel priority. First found vendor wins. Caches result to avoid repeated PATH lookups.

#### Services

- **Offline Magisk provisioning** (`andler-disk`): `provision_magisk()` installs Magisk root access into an Android overlay disk offline via `qemu-nbd`. RAII guards ensure cleanup on all error paths.
- **Snapshot metadata persistence** (`andler-store`): `snapshots` table with `ON DELETE CASCADE` from `instances`. Full CRUD for snapshot metadata.
- **gRPC snapshot operations** (`andler-rpc`): `CreateSnapshot`, `RestoreSnapshot`, `DeleteSnapshot`, `ListSnapshots` RPCs with per-operation timeout support.
- **gRPC metrics streaming** (`andler-rpc`): `StreamResourceMetrics` server-streaming RPC with `ResourceMetricsResponse` (all 9 optional fields including GPU).
- **gRPC Magisk provisioning** (`andler-rpc`): `magisk_dir` field on `CreateAndroidInstanceRequest` for offline root access.

#### Daemon

- **Snapshot orchestration**: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots` methods with FSM state validation and per-operation timeout override.
- **Metrics streaming**: `stream_resource_metrics` method returning `BoxStream<'static, ResourceMetrics>`.
- **Magisk provisioning integration**: `create_android_instance` accepts optional `magisk_dir` parameter.
- **Factory reset / `remove --purge`**: Full end-to-end with file cleanup (disk + OVMF vars). Refuses when live `Linked` clones exist.
- **Clone for LinuxVm**: `clone_instance` supports `Linked` and `FullStandalone` modes.
- **Export for LinuxVm**: `export_instance_disk` works for both AndroidVm and LinuxVm.
- **gRPC round-trip tests**: 25 integration tests with real TCP connections.

#### CLI

- **Unified `create` command**: Single command with `--kind linux`/`--kind android` discriminator. TOML mode auto-detects type from content.
- **Linux VM CLI args**: `--kind linux --name --iso-path --disk-path --ovmf-vars-template [--disk-size-gib]` creates LinuxVm without TOML.
- **Android VM from TOML**: `--file android.toml` with `android_version` field creates AndroidVm — no CLI flags needed.
- **`--magisk-dir` flag**: For offline Magisk provisioning.
- **Snapshot `--timeout` flag**: Override per-instance snapshot timeout for a single operation.
- **Metrics display**: GPU columns (VRAM, GPU%) when AMD/NVIDIA/Intel data available. Human-readable byte formatting.
- **Snapshot subcommands**: `create`, `restore`, `delete`, `list` under `andler snapshot`.
- **`clone` and `export`**: Commands for LinuxVm + AndroidVm.
- **Default XDG paths**: Instance data stored under `~/.local/share/andler/` by default.

### Changed

- **Monorepo restructure**: `crates/andler-*` reorganized into `core/`, `backends/`, `services/`, `daemon/`, `cli/` directories. Package names keep `andler-` prefix.
- **Documentation language**: All docs now in English. Historical/future docs moved to `docs/archive/`.

### Removed

- Dead stub crates: `frontend/`, `guest-image/`, `packaging/`, `presets/`
- Dead `--instance-kind` flag from CLI
- `create-android` command (merged into `create`)
- Russian-language documentation (moved to `docs/archive/`)

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
