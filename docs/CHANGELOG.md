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
- **Path utilities**: `runtime_dir()` (XDG_RUNTIME_DIR fallback), `current_uid()` (getuid FFI), `ensure_private_dir()` / `ensure_private_dir_sync()` (0700 permissions).
- **`ensure_qcow2_extension()`**: Auto-appends `.qcow2` extension to disk paths.

#### Backend (`andler-qemu`)

- **QEMU VM snapshots**: Full CRUD — create, restore, delete, list snapshots via QEMU's `snapshot-save`/`snapshot-load`/`snapshot-delete` job API. Polls `query-jobs` for completion with configurable timeout.
- **Resource metrics from `/proc`**: Real-time streaming of CPU%, RAM usage, disk I/O, and network I/O. No QMP required. 1-second polling interval.
- **GPU metrics (AMD)**: Sysfs-based GPU metrics — VRAM used/total and GPU load percentage from `/sys/class/drm/card*/device/`.
- **GPU metrics (NVIDIA)**: `nvidia-smi` CLI-based GPU metrics — VRAM used/total (MiB) and GPU load %. Automatic vendor detection with AMD→NVIDIA→Intel priority.
- **GPU metrics (Intel)**: i915 sysfs-based GPU metrics — GPU load % via `power/rc6_residency_ms` idle-time delta (documented i915 ABI; an earlier draft read a non-existent `busyiffies` path — never shipped). No VRAM metric — integrated Intel VRAM accounting isn't a stable sysfs ABI.
- **GPU vendor detection**: Automatic AMD → NVIDIA → Intel priority. First found vendor wins. Caches result to avoid repeated PATH lookups.
- **QMP connection recovery**: `pause`/`resume`/`status` no longer get stuck on a stale cached QMP connection after a transient disconnect — they clear it and reconnect once before failing. New `BackendError::ProcessNotRunning` distinguishes "QEMU process itself exited" (checked via `is_alive()`) from a recoverable QMP hiccup or a genuine command error (`CommandFailed`/`ParseError`, never retried).

#### Services

- **Offline Magisk provisioning** (`andler-disk`): `provision_magisk()` installs Magisk root access into an Android overlay disk offline via `qemu-nbd`. RAII guards ensure cleanup on all error paths.
- **Disk error variants**: `ShrinkRequiresConfirmation` (requires `--shrink` flag), `CompactNotApplicable` (non-qcow2 disk).
- **Snapshot metadata persistence** (`andler-store`): `snapshots` table with `ON DELETE CASCADE` from `instances`. Full CRUD for snapshot metadata.
- **gRPC snapshot operations** (`andler-rpc`): `CreateSnapshot`, `RestoreSnapshot`, `DeleteSnapshot`, `ListSnapshots` RPCs with per-operation timeout support.
- **gRPC metrics streaming** (`andler-rpc`): `StreamResourceMetrics` server-streaming RPC with `ResourceMetricsResponse` (all 9 optional fields including GPU).
- **gRPC Magisk provisioning** (`andler-rpc`): `magisk_dir` field on `CreateAndroidInstanceRequest` for offline root access.
- **gRPC config editing** (`andler-rpc`): `UpdateInstanceConfig` RPC with `UpdateInstanceConfigRequest` mirroring `GetInstanceConfigResponse` fields.
- **NVML integration** (`andler-firmware`): `nvml-wrapper` crate for NVIDIA GPU metrics (primary), with `nvidia-smi` CLI fallback.
- **Hardware auto-detection** (`andler-firmware`): `detect_all()` returns `HardwareDefaults` — GPU render backend, display engine, audio server, ARM translator, OVMF paths, Venus support, passt availability.
- **Network configuration service** (`andler-net`): `NetworkService` trait and `DefaultNetworkService` implementation for bridge and isolated network modes. Uses `iproute2` for host-side network setup.

#### Daemon

- **Snapshot orchestration**: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots` methods with FSM state validation and per-operation timeout override.
- **Metrics streaming**: `stream_resource_metrics` method returning `BoxStream<'static, ResourceMetrics>`.
- **Magisk provisioning integration**: `create_android_instance` accepts optional `magisk_dir` parameter.
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
- **New error variants**: `SnapshotLimitExceeded`, `MalformedInstanceRef`, `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged` (DaemonError: 23 variants total).
- **gRPC round-trip tests**: 24 integration tests with real TCP connections.

#### CLI

- **Unified `create` command**: Single command with `--kind linux`/`--kind android` discriminator. TOML mode auto-detects type from content.
- **Linux VM CLI args**: `--kind linux --name --iso-path --disk-path --ovmf-vars-template [--disk-size-gib]` creates LinuxVm without TOML.
- **Android VM from TOML**: `--file android.toml` with `android_version` field creates AndroidVm — no CLI flags needed.
- **`--magisk-dir` flag**: For offline Magisk provisioning.
- **Snapshot `--timeout` flag**: Override per-instance snapshot timeout for a single operation.
- **`andler disk` commands**: `create`, `info`, `resize`, `compact` for disk management. Flexible size format (`64GB`, `128000MB`, `1T`).
- **`disk resize --shrink`**: Explicit confirmation required for shrinking disks.
- **Metrics display**: GPU columns (VRAM, GPU%) when AMD/NVIDIA/Intel data available. Human-readable byte formatting.
- **Snapshot subcommands**: `create`, `restore`, `delete`, `list` under `andler snapshot`.
- **Guest package management**: `andler guest install/remove/list` — install, remove, or list known packages (`spice-vdagent`, `qemu-guest-agent`, `spice-webdavd`) in guest OS. Auto-fallback: online via QMP `guest-exec` if VM running, offline via `qemu-nbd` + mount if stopped.
- **`clone` and `export`**: Commands for LinuxVm + AndroidVm.
- **Default XDG paths**: Instance data stored under `~/.local/share/andler/` by default.
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
- **`create --verify`**: Validates a resolved instance config (paths exist, OVMF found/required-for-Android, disk size sane, GPU memory/CPU/memory in range) and prints a ✓/✗ report without contacting the daemon; exits non-zero if any check fails. Shares the same client-side resolution as `--dry-run` (`cli/src/preview.rs::resolve_linux`/`resolve_android`).

### Changed

- **Monorepo restructure**: `crates/andler-*` reorganized into `core/`, `backends/`, `services/`, `daemon/`, `cli/` directories. Package names keep `andler-` prefix.
- **Documentation language**: All docs now in English. Historical/future docs moved to `docs/archive/`.
- **Daemon module decomposition**: `daemon/src/daemon.rs` (3209 lines) decomposed into 9 files under `daemon/src/daemon/`. Core `mod.rs` reduced to 268 lines (92% reduction). Error types, instance lifecycle, clone/export, snapshots, and queries each in separate modules. Tests split into 9 domain-specific test files.
- **CLI module decomposition**: `cli/src/main.rs` (1185 lines) decomposed into 8 modules. Main dispatch reduced to 769 lines. Commands extracted to domain-specific files: `create.rs`, `edit.rs`, `status.rs`, `snapshot.rs`, `disk.rs`, `lifecycle.rs`, `clone.rs`, `helpers.rs`.
- **Disk default size**: 40 GiB → **256 GiB** (thin-provisioned qcow2, actual usage minimal).
- **Database filename**: `state.db` → **`andlerd.db`**.
- **Snapshot state requirements**: `restore`/`delete` now require **Running/Paused** instance (not terminal states) — QMP commands need live QEMU process.
- **`--libndk` → `--arm-translator`**: Boolean flag replaced by enum: `none`, `libndk`, `libhoudini`.
- **DaemonError expanded**: 14 → **23 variants** (added SnapshotLimitExceeded, MalformedInstanceRef, ConfigIdMismatch, ConfigKindChanged, ConfigDiskPathChanged, EmptyInstanceRef, InstanceRefNotFound, AmbiguousInstanceId).
- **Environment variables**: Added `ANDLERD_LISTEN_ADDR` (daemon listen), `ANDLERD_OVMF_CODE`/`ANDLERD_OVMF_VARS` (firmware override), `ANDLERD_LOG_FORMAT` (json output). `ANDLERD_ADDR` remains for CLI client.
- **`stop --graceful` semantics**: Now means "force stop without waiting for graceful ACPI shutdown" (flag name is historical; default behavior is graceful).
- **`purge_instance_files`**: Uses `remove_dir_all` for UUID-pattern instance directories (was `remove_dir`, silently failed on non-empty).
- **`provision_magisk`**: Entire pipeline wrapped in `tokio::task::spawn_blocking` (was blocking tokio executor with `std::thread::sleep`).
- **QemuBackend pause/resume**: Lock extracted before `backend.pause().await` (was held across await, blocking all operations).
- **Metrics output format**: `key=value` pairs (was columnar table headers).
- **Venus default on NVIDIA**: Skips Mesa version check for NVIDIA vendor (Venus uses Vulkan, not Mesa's OpenGL).
- **Clipboard documentation**: Wizard prints `spice-vdagent` install note when clipboard is enabled.
- **Resolution documentation**: Help text and doc comments note this isn't applied to SDL/GTK output yet.
- **Instance config persistence**: `write_instance_toml` writes TOML alongside instance (SQLite remains source of truth for restore).

### Fixed

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
  `daemon/src/grpc_roundtrip_test.rs`.
- **`andler-qemu` snapshot QMP wire protocol**: `snapshot-save`/`snapshot-load`/`snapshot-delete`
  were sending a singular `"device"` argument and, for save/load, omitting the required
  `"vmstate"` field — real QEMU (job-based API, 6.0+) expects a `"devices"` array plus
  `"vmstate"` for save/load, and rejects the malformed request immediately with `{"error": ...}`
  without ever starting the job. Fixed to send the correct schema.
- **`wait_job_completion` terminal status check**: was matching on `"completed"`/`"failed"`/
  `"aborted"`, none of which exist in QEMU's real job status enum (the only terminal status is
  `"concluded"`; success/failure is distinguished by the presence of an `error` field, not by a
  separate status value). This meant every snapshot operation — even a successful one — would
  poll until timeout rather than ever detecting completion. Fixed, and `job-dismiss` is now
  called after a job concludes (previously never called, leaving concluded jobs visible in
  `query-jobs` forever).
- **`execute_raw` no longer errors on async QMP events** (e.g. `JOB_STATUS_CHANGE`) received
  between sending a command and reading its reply — these can legitimately interleave with
  command/response traffic during job polling. Previously any such message was treated as a
  parse error.
- None of the above were caught by the existing test suite — `snapshot_save`/`load`/`delete` had
  no tests asserting on the actual JSON sent, and `wait_job_completion`'s tests encoded the same
  incorrect status strings as the implementation. New tests cover the real wire protocol using a
  `UnixStream::pair`-based fake QMP peer (see `andler-qemu/src/qmp.rs`).
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
- **`andler-disk` magisk provisioning** (`magisk.rs`): removed a pointless `unsafe` block around
  plain `SystemTime::now().duration_since(...)` (entirely safe Rust; the `unsafe` did nothing and
  the accompanying safety comment justified nothing real). `NbdGuard`/`MountGuard`'s `Drop` impls
  previously discarded the result of `qemu-nbd --disconnect`/`umount -l` entirely (`let _ = ...`)
  — neither a failed spawn nor a non-zero exit status was ever observed, which could leave
  `/dev/nbd*` devices connected indefinitely with no diagnostic trail. Both now log via
  `tracing::warn!` on failure (added `tracing` as a dependency of `andler-disk`, which it
  previously lacked). Also replaced `.to_str().unwrap()` with `.to_string_lossy()` in both `Drop`
  impls so a non-UTF-8 path can't turn an already-failing cleanup into a panic during unwind.
  Separately, found and fixed two stray CJK characters embedded in Russian-language doc comments
  in `magisk.rs` and `metrics.rs` (`分区`, `不同的`, `开场的`) — encoding/generation artifacts, not
  intentional text.

### Removed

- Dead stub crates: `frontend/`, `guest-image/`, `packaging/`, `presets/`
- Dead `--instance-kind` flag from CLI
- `create-android` command (merged into `create`)
- Russian-language documentation (moved to `docs/archive/`)
- `--libndk` boolean flag (replaced by `--arm-translator` enum)
- `KernelSU` root mode (proto value `3` reserved, cleaned from all layers)
- WIZARD.md (fully implemented, plan document removed)
- PLAN.md old content (replaced by current UX improvement plan)

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
