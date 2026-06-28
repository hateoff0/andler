# andler-qemu

QEMU hypervisor backend implementation. Fulfills the `HypervisorBackend` trait from `andler-core` by managing real `qemu-system-x86_64` processes, QMP communication, and `/proc`-based metrics collection.

## Modules

### `cmdline` — QEMU Command-Line Builder

Pure function `build_args(&InstanceConfig, &Path) -> Vec<String>` that translates the domain `InstanceConfig` into QEMU CLI arguments. Organized into blocks, each tested independently:

| Block | Function | What it generates |
|-------|----------|-------------------|
| Name | `name_args` | `-name <instance_name>` |
| Machine & CPU | `machine_and_cpu_args` | `-machine`, `-cpu`, `-smp`, `-enable-kvm` |
| Memory | `memory_args` | `-m`, `-mem-path`, `-mem-prealloc`, `-object memory-backend-memfd` |
| Firmware | `firmware_args` | `-drive if=pflash` for OVMF_CODE and OVMF_VARS |
| GPU & Display | `gpu_display_args` | `-device virtio-gpu-pci`, `-display`, `-vga` depending on `RenderBackend` and `DisplayEngine` |
| Disk | `disk_args` | `-drive file=...,format=qcow2,id=drive-disk0`, `-device virtio-blk-pci` |
| Input | `input_args` | `-device virtio-tablet-pci`, `-device virtio-keyboard-pci`, `-chardev qemu-vdagent` |
| Network | `network_args` | `-netdev user`, `-device virtio-net-pci` |
| Audio | `audio_args` | `-audiodev`, `-device` for PipeWire/PulseAudio |
| QMP | `qmp_args` | `-qmp unix:<path>,server,nowait` |

Always ends with `-boot menu=on`.

**RenderBackend mapping:**
- `Venus` → `-device virtio-gpu-pci,virgl=on,gfxpassthrough=on,hostmem=...`
- `VirtioGpu` → `-device virtio-gpu-pci` (no gl context)
- `VirGl` → `-device virtio-gpu-pci,virgl=on`
- `Cpu` → `-vga std` (software rendering)
- `Passthrough` → **panics** — `QemuBackend::spawn` rejects it before reaching `cmdline`

**DisplayEngine mapping:**
- `Sdl` → `-display sdl,gl=on`
- `Spice` → `-display spice-app`
- `Dbus` → `-display dbus`
- `None` → `-display none`

### `process` — QEMU Process Management

Low-level management of a real `qemu-system-x86_64` process via `tokio::process::Command`.

**`QemuProcess`**:

| Method | Signature | Description |
|--------|-----------|-------------|
| `spawn` | `async fn(args: &[String], qmp_socket_path: PathBuf) -> Result<Self, ProcessError>` | Spawn QEMU process, start log + metrics background tasks |
| `subscribe_logs` | `fn(&self) -> broadcast::Receiver<LogLine>` | Subscribe to stdout/stderr lines |
| `subscribe_metrics` | `fn(&self) -> broadcast::Receiver<ResourceMetrics>` | Subscribe to resource metrics |
| `pid` | `fn(&self) -> u32` | Process ID |
| `qmp_socket_path` | `fn(&self) -> &PathBuf` | Path to QMP unix socket |
| `is_alive` | `async fn(&mut self) -> Result<bool, ProcessError>` | Check if process is still running |
| `terminate` | `async fn(&mut self) -> Result<(), ProcessError>` | SIGTERM + 30s timeout (temporary until proper ACPI) |
| `force_kill` | `async fn(&mut self) -> Result<(), ProcessError>` | SIGKILL |

stdout/stderr are drained line-by-line in background tasks (`drain_to_tracing`): each line is simultaneously logged via `tracing::warn!` and published to a `broadcast` channel (`subscribe_logs`).

**`ProcessError`**: `SpawnFailed(io::Error)`, `Io(io::Error)`, `GracefulShutdownTimedOut(Duration)`.

**Constants**: `GRACEFUL_SHUTDOWN_TIMEOUT` = 30s, `LOG_CHANNEL_CAPACITY` = 256, `METRICS_CHANNEL_CAPACITY` = 64.

### `qmp` — QEMU Machine Protocol Client

QMP client over a unix socket. Handles handshake, command execution, and async job polling.

**`QmpClient`**:

| Method | Signature | Description |
|--------|-----------|-------------|
| `connect` | `async fn(socket_path: &Path) -> Result<Self, QmpError>` | Connect + QMP handshake + `qmp_capabilities` |
| `pause` | `async fn(&mut self) -> Result<(), QmpError>` | Sends `stop` command |
| `resume` | `async fn(&mut self) -> Result<(), QmpError>` | Sends `cont` command |
| `query_status` | `async fn(&mut self) -> Result<VmStatus, QmpError>` | Sends `query-status` |
| `snapshot_save` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-save` job |
| `snapshot_load` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-load` job |
| `snapshot_delete` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-delete` job |
| `wait_job_completion` | `async fn(&mut self, job_id, timeout) -> Result<(), QmpError>` | Polls `query-jobs` until done |
| `query_block_snapshots` | `async fn(&mut self, device) -> Result<Vec<SnapshotInfo>, QmpError>` | Lists snapshots for a block device |

**`VmStatus`**: `Running` | `Paused` | `Shutdown` | `Other`.

**`QmpError`**: `ConnectFailed`, `Io`, `ConnectionClosed`, `ParseError`, `CommandFailed { command, class, desc }`.

### `backend` — QemuBackend Implementation

**`QemuBackend`**: `HypervisorBackend` implementation tying together `cmdline`, `process`, and `qmp`.

- `instances: Mutex<HashMap<BackendHandle, RunningInstance>>` — registry of live instances.
- Each `RunningInstance` holds: `process: QemuProcess`, `qmp_client: Option<QmpClient>` (lazy-connected on first QMP operation), `snapshot_timeout: Duration` (from `DiskConfig::snapshot_timeout_secs`, default 30s).

**`QmpClient` lazy connection**: QMP socket isn't ready immediately after `spawn`. The client connects on first `pause`/`resume`/`status`/`snapshot` call via `ensure_qmp_connected`.

**Snapshot device name**: Hardcoded as `drive-disk0` (matches `cmdline.rs`). If multi-disk support is added in the future, this must become a parameter.

### `metrics` — Host Metrics Collection

Collects resource metrics from `/proc` for the QEMU process. No QMP needed.

**Public API**:

| Function | Signature | Description |
|----------|-----------|-------------|
| `read_metrics_sample` | `fn(pid: u32) -> Option<ResourceMetrics>` | Single-point read of all metrics |
| `spawn_metrics_poller` | `fn(pid, ticks_per_sec, interval, sender) -> JoinHandle<()>` | Background task that reads metrics every `interval` and sends via `broadcast::Sender` |
| `ticks_per_second` | `fn() -> u64` | Reads `sysconf(_SC_CLK_TCK)` for CPU% calculation |

**Data sources**:
- **CPU%**: `/proc/<pid>/stat` fields utime (index 11) + stime (index 12), delta-based: `delta(utime+stime) / delta(uptime) / num_cpus * 100`
- **RAM**: `/proc/<pid>/status` VmRSS field
- **Disk I/O**: `/sys/block/<dev>/stat` read/write sectors + time
- **Net I/O**: `/proc/<net/dev>` rx/tx bytes delta

**Polling interval**: 1 second (`DEFAULT_POLL_INTERVAL`).

### `gpu_metrics` — GPU Metrics (AMD sysfs)

Reads GPU metrics from the Linux DRM sysfs subsystem. **AMD-only** — other vendors return `None`.

**Public API**:

| Function | Signature | Description |
|----------|-----------|-------------|
| `read_gpu_metrics` | `fn() -> ResourceMetrics` | Reads AMD sysfs files, returns metrics with VRAM + GPU load |
| `merge_gpu_metrics` | `fn(base: &mut ResourceMetrics, gpu: &ResourceMetrics)` | Merges GPU fields into base metrics (fills `None` fields, doesn't overwrite existing) |

**Sysfs paths** (first AMD card found under `/sys/class/drm/card*/device/`):
- `mem_info_vram_used` → `vram_used_bytes`
- `mem_info_vram_total` → `vram_total_bytes`
- `gpu_busy_percent` → `gpu_load_percent`

**Integration**: `spawn_metrics_poller` in `metrics.rs` calls `read_gpu_metrics()` and merges into the base metrics sample every tick. Single merged `ResourceMetrics` message per tick — no separate GPU channel.

## Tests

50 tests across 6 test modules.

### Without `/dev/kvm` or QEMU binary

- **`cmdline`** (22 tests): All argument blocks tested independently against `scripts/start.sh` reference. Includes edge cases: `Passthrough` panic, `None` display engine, clipboard disabled, size suffixes.
- **`qmp`** (12 tests): JSON parsing of QMP responses (`QmpReply`, `VmStatus`, `QueryStatusReturn`, `SnapshotInfo`, `QueryJobInfo`).
- **`backend`** (9 tests): `Passthrough` validation, unknown handle handling, `NotImplemented` branches, `VmStatus → InstanceState` mapping, empty `metrics_stream`/`log_stream`.
- **`process`** (3 tests): `SpawnFailed` via missing binary, `drain_to_tracing` line publishing, subscriber tolerance.
- **`metrics`** (7 tests): CPU stat parsing, CPU% computation, I/O rates, RSS parsing, net_dev parsing.
- **`gpu_metrics`** (5 tests): No DRM fallback, panic safety, merge behavior.

### With `/dev/kvm` and `qemu-system-x86_64` (integration tests, `#[ignore]`)

- `spawn_then_status_then_stop_round_trip`
- `spawn_then_pause_then_resume_round_trip`
- `log_stream_receives_real_process_output`
- `connect_then_pause_then_resume_round_trip` (QMP)
- `spawn_then_is_alive_then_terminate`
- `force_kill_stops_unresponsive_process`
- `create_then_virtual_size_round_trips`
- `create_with_backing_file_fails_fast_on_missing_backing`

All marked `#[ignore]` with reason — run separately in `integration-test` Docker target.

## Known Limitations

- **`status()` without live QMP**: If QMP socket isn't ready (just spawned, or connection dropped), degrades to "process alive, exact status unknown" — reflected in `detail`, not silently assumed `Running`.
- **No QMP event queue**: `qmp.rs` doesn't distinguish asynchronous events from command responses. Not a problem for current scope (stop/cont/query-status don't generate client-relevant events), but will be a limitation when event subscription is added.
- **Hardcoded snapshot device name**: `drive-disk0`. Will need parameterization if multi-disk support is added.
- **Snapshot timeout**: Configurable per-instance via `DiskConfig::snapshot_timeout_secs` (default 30s). For very large snapshots (hundreds of GiB), this may need per-operation tuning.
- **Network modes**: `NetworkMode::Bridge`/`Isolated` panic in `network_args` — `andler-net` hasn't implemented them yet. This is an explicit refusal, not a silent NAT fallback.

## What Is NOT Implemented Here

- `RenderBackend::Passthrough` (VFIO GPU passthrough) — `gpu_display_args` panics, but `QemuBackend::spawn` checks `RenderBackend::is_implemented()` first and returns `BackendError::InvalidConfig`, never reaching `gpu_display_args` in normal flow.
- `NetworkMode::Bridge`/`Isolated` — panic in `network_args` until `andler-net` implements them.
