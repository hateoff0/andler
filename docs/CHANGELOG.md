# Changelog

All notable changes to ANDLER will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

#### Core (`andler-core`)

- **Unified `create` command**: Merged `create` and `create-android` into a single `create` command. `--android-version` discriminator selects AndroidVm mode; without it, `--file` creates a LinuxVm from a TOML config.
- **Snapshot timeout per instance**: `DiskConfig::snapshot_timeout_secs` allows configuring the async snapshot job timeout per-instance (default 30s).
- **LinuxVm clone/export**: `CloneMode::Linked` and `CloneMode::FullStandalone` supported for LinuxVm. `SharedBase` rejected with `SharedBaseNotSupportedForLinuxVm`.

#### Backend (`andler-qemu`)

- **QEMU VM snapshots**: Full CRUD — create, restore, delete, list snapshots via QEMU's `snapshot-save`/`snapshot-load`/`snapshot-delete` job API. Polls `query-jobs` for completion with configurable timeout.
- **Resource metrics from `/proc`**: Real-time streaming of CPU%, RAM usage, disk I/O, and network I/O. No QMP required for metrics. 1-second polling interval.
- **GPU metrics (AMD)**: Sysfs-based GPU metrics — VRAM used/total and GPU load percentage from `/sys/class/drm/card*/device/`. Other vendors return None (explicit first-version constraint).
- **GPU metrics integration**: AMD GPU metrics merged into the main metrics poller. Single `ResourceMetrics` message per tick with both host and GPU data.
- **Configurable snapshot timeout**: Per-instance `snapshot_timeout` field in `RunningInstance`, read from `DiskConfig::snapshot_timeout_secs`.

#### Services

- **Offline Magisk provisioning** (`andler-disk`): `provision_magisk()` installs Magisk root access into an Android overlay disk offline via `qemu-nbd`. RAII guards (`NbdGuard`, `MountGuard`) ensure cleanup on all error paths. Requires `nbd` kernel module.
- **Snapshot metadata persistence** (`andler-store`): `snapshots` table with `ON DELETE CASCADE` from `instances`. Full CRUD for snapshot metadata (tag, description, created_at).
- **gRPC snapshot operations** (`andler-rpc`): `CreateSnapshot`, `RestoreSnapshot`, `DeleteSnapshot`, `ListSnapshots` RPCs with full proto definitions and conversions.
- **gRPC metrics streaming** (`andler-rpc`): `StreamResourceMetrics` server-streaming RPC with `ResourceMetricsResponse` (all 9 optional fields including GPU).
- **gRPC Magisk provisioning** (`andler-rpc`): `magisk_dir` field on `CreateAndroidInstanceRequest` for offline root access.

#### Daemon

- **Snapshot orchestration**: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots` methods with FSM state validation (Running/Paused for create, Stopped for restore/delete).
- **Metrics streaming**: `stream_resource_metrics` method returning `BoxStream<'static, ResourceMetrics>`.
- **Magisk provisioning integration**: `create_android_instance` accepts optional `magisk_dir` parameter, calls `provision_magisk` when provided.
- **Factory reset / `remove --purge`**: Full end-to-end with file cleanup (disk + OVMF vars + best-effort parent dir removal). Refuses when live `Linked` clones exist.
- **Clone for LinuxVm**: `clone_instance` supports `Linked` and `FullStandalone` modes for LinuxVm.
- **Export for LinuxVm**: `export_instance_disk` works for both AndroidVm and LinuxVm.
- **gRPC round-trip tests**: 25 integration tests with real TCP connections.

#### CLI

- **Unified `create` command**: Single command with `--android-version` discriminator.
- **`--magisk-dir` flag**: For offline Magisk provisioning.
- **Metrics display**: GPU columns (VRAM, GPU%) when AMD data available. Human-readable byte formatting.
- **Snapshot subcommands**: `create`, `restore`, `delete`, `list` under `andler snapshot`.
- **`clone` and `export`**: Commands for LinuxVm + AndroidVm.
- **Default XDG paths**: Instance data stored under `~/.local/share/andler/` by default.

### Changed

- **Monorepo restructure**: `crates/andler-*` reorganized into `core/`, `backends/`, `services/`, `daemon/`, `cli/` directories. Package names keep `andler-` prefix to avoid `core` shadowing with Rust std.
- **Snapshot timeout**: Now configurable per-instance via `DiskConfig::snapshot_timeout_secs` (default 30s) instead of hardcoded 30s.
- **Metrics merge**: GPU metrics integrated into existing `spawn_metrics_poller` instead of separate channel.
- **Documentation language**: All docs now in English. Historical/future docs moved to `docs/archive/`.
- **README updates**: All 9 crate READMEs rewritten in English, matching actual code state. Project-level docs (ARCHITECTURE, DEVELOPMENT, API, CHANGELOG) expanded.

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
- 22 unit tests in `andler-core`
- 50 tests in `andler-qemu`
- 19 tests in `andler-store`
- 27 tests in `andler-rpc`
- 65+ tests in `andler-daemon`
- 7 tests in `andler-cli`
