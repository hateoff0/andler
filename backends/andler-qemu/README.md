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
| `wait_job_completion` | `async fn(&mut self, job_id, timeout) -> Result<(), QmpError>` | Polls `query-jobs` until `"concluded"`, then `job-dismiss` |
| `query_block_snapshots` | `async fn(&mut self, device) -> Result<Vec<SnapshotInfo>, QmpError>` | Lists snapshots for a block device |

**`VmStatus`**: `Running` | `Paused` | `Shutdown` | `Other`.

**`QmpError`**: `ConnectFailed`, `Io`, `ConnectionClosed`, `ParseError`, `CommandFailed { command, class, desc }`.

**QMP wire schema for `snapshot-save`/`-load`/`-delete` (job-based API, QEMU 6.0+)** — this is
the part that's easy to get wrong and previously *was* wrong in this crate, so it's documented
here explicitly:

```text
snapshot-save:   {"job-id": ..., "tag": ..., "vmstate": <node-name>, "devices": [<node-name>, ...]}
snapshot-load:   {"job-id": ..., "tag": ..., "vmstate": <node-name>, "devices": [<node-name>, ...]}
snapshot-delete: {"job-id": ..., "tag": ..., "devices": [<node-name>, ...]}               (no vmstate)
```

`devices` is a **list**, not a singular `device` field — QEMU rejects the command outright
(`{"error": ...}`, job never starts) if you send `device` instead. `snapshot-save`/`-load` also
require `vmstate` (the node where CPU/RAM state is stored); `snapshot-delete` does not. This
crate's `device: &str` parameter on `snapshot_save`/`snapshot_load`/`snapshot_delete` is used to
build both `vmstate` and the single-element `devices` array internally — the public method
signatures didn't need to change, only the JSON they construct.

**Job status polling** — QEMU's job state machine has exactly one terminal status,
`"concluded"` (`created`/`running`/`paused`/`ready`/`standby`/`waiting`/`pending`/`aborting` are
all non-terminal). There is no `"completed"`/`"failed"`/`"aborted"` status string in the real
protocol — an earlier version of `wait_job_completion` checked for those, which meant it always
fell through to the timeout branch even on a successful snapshot. Success vs. failure of a
`"concluded"` job is distinguished by the presence of an `error` field, not by a different status
value. After a job reaches `"concluded"` (either outcome), `job-dismiss` must be called
explicitly — otherwise it stays visible in `query-jobs` forever.

**Event skipping** — `JOB_STATUS_CHANGE` (and other) events can arrive on the same socket
between sending a command and receiving its reply, especially during the `query-jobs` polling
loop. `execute_raw` reads through any message lacking `return`/`error` (i.e. an event) via
`read_reply_skipping_events` instead of treating it as a parse error.

### `backend` — QemuBackend Implementation

**`QemuBackend`**: `HypervisorBackend` implementation tying together `cmdline`, `process`, and `qmp`.

- `instances: Mutex<HashMap<BackendHandle, RunningInstance>>` — registry of live instances.
- Each `RunningInstance` holds: `process: QemuProcess`, `qmp_client: Option<QmpClient>` (lazy-connected on first QMP operation), `snapshot_timeout: Duration` (from `DiskConfig::snapshot_timeout_secs`, default 30s).

**`QmpClient` lazy connection**: QMP socket isn't ready immediately after `spawn`. The client connects on first `pause`/`resume`/`status`/`snapshot` call via `ensure_qmp_connected`.

**Snapshot device name**: Hardcoded as `drive-disk0` (matches `cmdline.rs`). If multi-disk support is added in the future, this must become a parameter. This same name is used both as the `vmstate` node and as the sole entry of `devices` — there's only one disk, so there's nowhere else to put the VM state.

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

### `gpu_metrics` — GPU Metrics (AMD, NVIDIA, Intel)

Reads GPU metrics from host-side sysfs and vendor CLI tools. Supports three vendors with automatic detection:

| Vendor | Source | Metrics |
|--------|--------|---------|
| AMD | sysfs `mem_info_vram_*`, `gpu_busy_percent` | VRAM used/total, GPU load % |
| NVIDIA | `nvidia-smi` CLI | VRAM used/total, GPU load % |
| Intel | sysfs `i915` `power/rc6_residency_ms` | GPU load % (idle-time-based), no VRAM |

**Public API**:

| Function | Signature | Description |
|----------|-----------|-------------|
| `read_gpu_metrics` | `fn() -> ResourceMetrics` | Detects vendor (AMD → NVIDIA → Intel) and reads metrics |
| `merge_gpu_metrics` | `fn(base: &mut ResourceMetrics, gpu: &ResourceMetrics)` | Merges GPU fields into base metrics (fills `None` fields, doesn't overwrite existing) |

**Vendor detection**: AMD → NVIDIA → Intel priority. First found vendor wins. `is_nvidia_available()` checks if `nvidia-smi` is in PATH.

**AMD sysfs paths** (first card under `/sys/class/drm/card*/device/`):
- `mem_info_vram_used` → `vram_used_bytes`
- `mem_info_vram_total` → `vram_total_bytes`
- `gpu_busy_percent` → `gpu_load_percent`

**NVIDIA**: Runs `nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv,noheader,nounits`. Values in MiB, converted to bytes. If `nvidia-smi` not found, returns None (no retry).

**Intel sysfs path** (i915 driver): `device/power/rc6_residency_ms` — cumulative
milliseconds spent in the RC6 idle power state since boot, a long-standing
documented i915 ABI (kept as a top-level compat path pointing at `gt/gt0/`
even after the upstream per-tile `gt/` sysfs reorganization). GPU load % is
derived as `100 - (Δrc6_residency_ms / Δwall_clock_ms * 100)` between two
calls, using a real elapsed-time delta (`Instant`), not an assumed fixed
polling interval. **No VRAM metric for Intel** — integrated Intel GPUs share
system RAM via "stolen memory" accounting that lives in `debugfs`, not a
stable `sysfs` ABI, so `vram_used_bytes`/`vram_total_bytes` are always `None`.

> An earlier version of this crate read a sysfs path that doesn't exist —
> `device/gt/gt0/attrs/busyiffies` for load, and `mem_info_dev_local_mem_alloc`/
> `mem_info_stolen_local_mem` for VRAM. None of those three paths are present
> anywhere in the real i915 sysfs tree (checked against `i915_sysfs.c` and the
> upstream `gt` sysfs reorganization commit). The bug shipped silently because
> the unit tests only exercised the delta-calculation arithmetic with
> hand-picked numbers, never the sysfs path string itself — on real Intel
> hardware, `read_intel_metrics` would have always returned `None` for both
> load and VRAM, with no error. Fixed to use the documented `rc6_residency_ms`
> ABI for load, and to honestly return `None` for VRAM rather than read from
> a path that was never real.

**Integration**: `spawn_metrics_poller` in `metrics.rs` calls `read_gpu_metrics()` and merges into the base metrics sample every tick. Single merged `ResourceMetrics` message per tick — no separate GPU channel.

## Tests

### Without `/dev/kvm` or QEMU binary

- **`cmdline`** (22 tests): All argument blocks tested independently against `scripts/start.sh` reference. Includes edge cases: `Passthrough` panic, `None` display engine, clipboard disabled, size suffixes.
- **`qmp`**: JSON parsing of QMP responses (`QmpReply`, `VmStatus`, `QueryStatusReturn`, `SnapshotInfo`, `QueryJobInfo`), plus `UnixStream::pair`-based fake-QMP-peer tests covering the real `snapshot-save`/`-load`/`-delete` wire schema (`devices`+`vmstate`, not the previously-buggy singular `device`), `wait_job_completion`'s `"concluded"`+`error` semantics (not the nonexistent `"completed"`/`"failed"`/`"aborted"` strings an earlier version checked), `job-dismiss`, and async-event skipping during polling.
- **`backend`** (9 tests): `Passthrough` validation, unknown handle handling, `NotImplemented` branches, `VmStatus → InstanceState` mapping, empty `metrics_stream`/`log_stream`.
- **`process`** (3 tests): `SpawnFailed` via missing binary, `drain_to_tracing` line publishing, subscriber tolerance.
- **`metrics`** (7 tests): CPU stat parsing, CPU% computation, I/O rates, RSS parsing, net_dev parsing.
- **`gpu_metrics`**: AMD/NVIDIA/Intel detection, NVIDIA output parsing, a regression test asserting the Intel sysfs path is the real `rc6_residency_ms` ABI (not `busyiffies`), the `intel_gpu_load_from_delta` pure-arithmetic core tested deterministically (fully idle/fully busy/partial/clamping/non-positive-elapsed) without sleeping or mocking `Instant`, plus a real-time-based `compute_intel_gpu_load` round-trip test, merge behavior, panic safety.

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
