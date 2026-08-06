# andler-qemu

QEMU hypervisor backend implementation. Fulfills the `HypervisorBackend` trait from `andler-core` by managing real `qemu-system-x86_64` processes, QMP communication, and `/proc`-based metrics collection.

## Modules

### `cmdline` — QEMU Command-Line Builder

Pure function `build_args(&InstanceConfig, &Path) -> Vec<String>` that translates the domain `InstanceConfig` into QEMU CLI arguments. Organized into blocks, each tested independently:

| Block | Function | What it generates |
|-------|----------|-------------------|
| Name | `name_args` | `-name <instance_name>,process=<instance_name>` |
| Machine & CPU | `machine_and_cpu_args` | `-machine`, `-cpu ... migratable=no`, `-smp`, `-enable-kvm` |
| Memory | `memory_args` | `-m <size>`, `-object memory-backend-memfd,...`, `-machine memory-backend=mem1` |
| Firmware | `firmware_args` | `-drive if=pflash` for OVMF_CODE and OVMF_VARS |
| GPU & Display | `gpu_display_args` | `-device virtio-gpu-pci`, `-display`, `-vga` depending on `RenderBackend` and `DisplayEngine` |
| Guest agent | `guest_agent_args` | `-chardev socket,id=qga,path=<...>/qmp.qga.sock,server=on,wait=off` + `-device virtserialport,chardev=qga,id=qga,name=org.qemu.guest_agent.0` (reuses the `virtio-serial-pci` bus from `input_args`) |
| Display resolution | `display_resolution_fwcfg_args` | `-fw_cfg name=opt/andler/display-resolution,string=<W>x<H>` when a resolution is configured (read at boot by the guest oneshot `andler-display-resolution.service`) |
| Disk | `disk_args` | `-drive file=...,format=qcow2,id=drive-disk0`, `-device virtio-blk-pci` |
| Input | `input_args` | `-device virtio-tablet-pci` (or `virtio-mouse-pci`), `-device virtio-serial-pci`, `-device virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0`, `-chardev qemu-vdagent` (when clipboard enabled) |
| Network | `network_args` | `-nic user,model=virtio-net-pci` (Slirp/NAT), `-netdev passt` + `-device` (Passt), or Bridge/Isolated modes |
| Audio | `audio_args` | `-audiodev`, `-device` for PipeWire/PulseAudio |
| QMP | `qmp_args` | `-qmp unix:<path>,server,nowait` |
| Serial | `serial_args` | `-serial file:console.log` (instance directory) |

Always ends with `-boot menu=on`.
**QEMU flag rationale**:
- `-machine q35,accel=kvm,usb=on` — q35 chipset with USB support
- `-cpu host,kvm=on,+topoext,migratable=no` — `+topoext` enables topology extension for multi-core reporting; `migratable=no` prevents live migration (not supported)
- `dies=1` hardcoded — `CpuConfig` doesn't have a `dies` field; multiple dies are not supported
- `-drive ... discard=on,detect-zeroes=on,aio=threads` — `discard=on` issues TRIM to QEMU for reclaimed blocks; `detect-zeroes=on` detects zero-filled regions; `aio=threads` uses thread-based async I/O (not native AIO, which has kernel version issues)
- `virtio-blk-pci,num-queues=4` — 4 virtio queues for better multi-core I/O parallelism

**RenderBackend mapping:**
- `Venus` → `-device virtio-gpu-gl,hostmem=...,blob=...,venus=true`
- `VirtioGpu` → `-device virtio-gpu-pci` (no gl context)
- `VirGl` → `-device virtio-gpu-gl,hostmem=...,blob=...`
- `Cpu` → `-vga std` (software rendering)
- `Passthrough` → **panics** — `QemuBackend::spawn` rejects it before reaching `cmdline`

**DisplayEngine mapping:**
- `Sdl` → `-display sdl,gl=on|off,show-cursor=on|off,window-close=off`
- `Gtk` → `-display gtk,gl=on|off,show-cursor=on|off,clipboard=on,window-close=off`
- `Spice` → `-display spice-app`
- `Dbus` → `-display dbus`
- `None` → `-display none`

### `process` — QEMU Process Management

Low-level management of a real `qemu-system-x86_64` process via `tokio::process::Command`.

**`QemuProcess`:**

| Method | Signature | Description |
|--------|-----------|-------------|
| `spawn` | `async fn(args: &[String], qmp_socket_path: PathBuf, log_file_path: Option<PathBuf>) -> Result<Self, ProcessError>` | Spawn QEMU process, start log + metrics background tasks |
| `subscribe_logs` | `fn(&self) -> broadcast::Receiver<LogLine>` | Subscribe to stdout/stderr lines |
| `subscribe_metrics` | `fn(&self) -> broadcast::Receiver<ResourceMetrics>` | Subscribe to resource metrics |
| `pid` | `fn(&self) -> u32` | Process ID |
| `qmp_socket_path` | `fn(&self) -> &PathBuf` | Path to QMP unix socket |
| `is_alive` | `async fn(&mut self) -> Result<bool, ProcessError>` | Check if process is still running |
| `terminate` | `async fn(&mut self) -> Result<(), ProcessError>` | SIGTERM + 30s timeout (temporary until proper ACPI) |
| `force_kill` | `async fn(&mut self) -> Result<(), ProcessError>` | SIGKILL |
| `log_file_path` | `fn(&self) -> Option<&Path>` | Path to the log file, if configured |

stdout/stderr are drained line-by-line in background tasks (`drain_to_tracing`): each line is simultaneously logged via `tracing::warn!` and published to a `broadcast` channel (`subscribe_logs`). Log lines use `[stdout]`/`[stderr]` prefixes (e.g. `[stdout] QEMU 8.2.0 starting`). `read_log_history()` parses these prefixes from the log file, and `log_stream` replays history from this file.

**`ProcessError`**: `SpawnFailed(io::Error)`, `Io(io::Error)`, `GracefulShutdownTimedOut(Duration)`.

**Constants**: `GRACEFUL_SHUTDOWN_TIMEOUT` = 30s, `LOG_CHANNEL_CAPACITY` = 256, `METRICS_CHANNEL_CAPACITY` = 64.

### `qmp` — QEMU Machine Protocol Client

QMP client over a unix socket. Handles handshake, command execution, and async job polling.

**`QmpClient`:**

| Method | Signature | Description |
|--------|-----------|-------------|
| `connect` | `async fn(socket_path: &Path) -> Result<Self, QmpError>` | Connect + QMP handshake + `qmp_capabilities` |
| `connect_agent` | `async fn(socket_path: &Path) -> Result<Self, QmpError>` | Connect to the **guest agent** chardev socket (`*.qga.sock`) — no QMP handshake; the agent speaks the `guest-*` JSON protocol directly |
| `pause` | `async fn(&mut self) -> Result<(), QmpError>` | Sends `stop` command |
| `resume` | `async fn(&mut self) -> Result<(), QmpError>` | Sends `cont` command |
| `query_status` | `async fn(&mut self) -> Result<VmStatus, QmpError>` | Sends `query-status` |
| `snapshot_save` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-save` job |
| `snapshot_load` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-load` job |
| `snapshot_delete` | `async fn(&mut self, device, tag) -> Result<String, QmpError>` | Async `snapshot-delete` job |
| `wait_job_completion` | `async fn(&mut self, job_id, timeout) -> Result<(), QmpError>` | Polls `query-jobs` until `"concluded"`, then `job-dismiss` |
| `guest_ping` | `async fn(&mut self) -> Result<(), QmpError>` | Check if guest agent is reachable (agent socket only) |
| `guest_exec` | `async fn(&mut self, path: &str, args: &[String]) -> Result<u64, QmpError>` | Execute command in guest (agent socket only) |
| `guest_exec_status` | `async fn(&mut self, pid: u64) -> Result<GuestExecStatus, QmpError>` | Poll guest-exec-status for a previously-started guest command |
| `guest_file_write` | `async fn(&mut self, path: &str, content: &str) -> Result<(), QmpError>` | Write a whole file in the guest via `guest-file-open`/`guest-file-write`/`guest-file-close` (wire param is `buf-b64`, not `data-b64`) |
| `is_guest_agent_available` | `async fn(&mut self) -> bool` | Returns `true` if guest agent responds to `guest-ping` |
| `query_block_snapshots` | `async fn(&mut self, device) -> Result<Vec<SnapshotInfo>, QmpError>` | Lists snapshots for a block device |

**`VmStatus`**: `Running` | `Paused` | `Shutdown` | `Other`.

**`QmpError`**: `ConnectFailed { path: String, source: std::io::Error }`, `Io`, `ConnectionClosed`, `ParseError`, `CommandFailed { command, class, desc }`.

**`GuestExecStatus`**: `exitcode: i64`, `exited: bool`, `out_data: String`, `err_data: String`.

**QMP wire schema for `snapshot-save`/`-load`/`-delete` (job-based API, QEMU 6.0+)**:

```text
snapshot-save:   {"job-id": ..., "tag": ..., "vmstate": <node-name>, "devices": [<node-name>, ...]}
snapshot-load:   {"job-id": ..., "tag": ..., "vmstate": <node-name>, "devices": [<node-name>, ...]}
snapshot-delete: {"job-id": ..., "tag": ..., "devices": [<node-name>, ...]}               (no vmstate)
```

`devices` is a **list**, not a singular `device` field — QEMU rejects the command outright (`{"error": ...}`, job never starts) if you send `device` instead. `snapshot-save`/`-load` also require `vmstate` (the node where CPU/RAM state is stored); `snapshot-delete` does not. This crate's `device: &str` parameter on `snapshot_save`/`snapshot_load`/`snapshot_delete` is used to build both `vmstate` and the single-element `devices` array internally.

**Job status polling** — QEMU's job state machine has exactly one terminal status, `"concluded"` (`created`/`running`/`paused`/`ready`/`standby`/`waiting`/`pending`/`aborting` are all non-terminal). Success vs. failure of a `"concluded"` job is distinguished by the presence of an `error` field, not by a different status value. After a job reaches `"concluded"` (either outcome), `job-dismiss` must be called explicitly — otherwise it stays visible in `query-jobs` forever.

**Package manager auto-detection**: `guest_exec_package` (used by guest package installation/removal) runs a detection script inside the guest via the agent (`connect_agent` on `*.qga.sock`): `command -v apt-get → exit 0`, `command -v dnf → exit 10`, `command -v pacman → exit 20`, else exit 30 (error "no supported package manager found"). The detected manager then runs `install -y` / `remove -y` (pacman: `-S --noconfirm` / `-R --noconfirm`). A non-zero exit propagates `GuestExecFailed` with the decoded stderr.
**Agent socket exclusivity** — the agent connect is established lazily per operation by `backend::guest_agent_client`. A QEMU chardev delivers its data to the *last* client only, so concurrent guest operations against the same `*.qga.sock` must serialize; never hold the socket across long operations.
**Event skipping** — `JOB_STATUS_CHANGE` (and other) events can arrive on the same socket between sending a command and receiving its reply, especially during the `query-jobs` polling loop. `execute_raw` reads through any message lacking `return`/`error` (i.e. an event) via `read_reply_skipping_events` instead of treating it as a parse error.

### `backend` — QemuBackend Implementation

**`QemuBackend`**: `HypervisorBackend` implementation tying together `cmdline`, `process`, and `qmp`.

- `instances: Mutex<HashMap<BackendHandle, RunningInstance>>` — registry of live instances.
- Each `RunningInstance` holds: `process: QemuProcess`, `qmp_client: Option<QmpClient>` (lazy-connected on first QMP operation), `snapshot_timeout: Duration` (from `DiskConfig::snapshot_timeout_secs`, default 30s).


**QMP socket directory**: Uses `andler_core::paths::runtime_dir()` → `$XDG_RUNTIME_DIR` on most Linux hosts (typically `/run/user/<uid>`), not `/tmp` — on multi-user systems `/tmp` is world-writable and vulnerable to symlink attacks on IPC sockets. The directory is created with 0700 permissions via `ensure_private_dir` in `spawn`.
**`QmpClient` lazy connection**: QMP socket isn't ready immediately after `spawn`. The client connects on first `pause`/`resume`/`status`/`snapshot` call via `ensure_qmp_connected`.
**Guest agent connection** (`guest_agent_client`): guest-package install/remove (`guest_install_package`/`guest_remove_package`) and `set_guest_display_resolution` connect a *separate* `QmpClient` to the QGA chardev socket (`process.qga_socket_path()` → `<qmp>.qga.sock`) with `connect_agent`. No `qmp_capabilities` handshake; the agent accepts `guest-*` JSON-RPC directly. The connection is not cached across calls — each guest operation connects fresh (a stale agent socket connection may be closed by QEMU when a newer client attaches).
**`set_guest_display_resolution`**: writes `/etc/andler/display.conf` (`RESOLUTION=<W>x<H>\n`) in the guest via `guest_file_write`, then applies it without rebooting — on Android it runs `systemctl restart waydroid-compositor.service`, on Linux `/usr/local/bin/andler-apply-resolution` (waits up to 30s via `guest-exec-status`). The guest-side machinery is provided by the base-image units (`andler-display-resolution.service`, `andler-apply-resolution`, `andler-pipewire*.service` — see `docker/images`).


**QMP error handling and recovery**: The `diagnose_and_reset_qmp` function handles connection-level failures (`Io`/`ConnectionClosed`/`ConnectFailed`) — `ensure_qmp_connected` checks both `is_some()` and that the cached connection actually works. Connection-level failures are handled by:
- **QEMU process exited** → clears stale client, returns `ProcessNotRunning` error
- **Process alive, connection dropped** (transient) → clears stale client, returns `None` to signal retry

Only connection-level errors trigger this; `CommandFailed`/`ParseError` mean QEMU answered (just with an error), so retry wouldn't help.

**`BackendHandle` format**: Currently `"qemu:{instance_id}"` — this is an implementation detail. Callers should treat it as opaque, obtained from `spawn()` and passed back as-is.
**Snapshot device name**: Hardcoded as `drive-disk0` (matches `cmdline.rs`). If multi-disk support is added in the future, this must become a parameter. This same name is used both as the `vmstate` node and as the sole entry of `devices` — there's only one disk, so there's nowhere else to put the VM state.
**QMP reconnection on transient failures**: `pause()` and `resume()` (and similar QMP operations) handle connection-level errors (`Io` / `ConnectionClosed`) gracefully: on error, the backend calls `diagnose_and_reset_qmp()`, checks process liveness, reconnects to the QMP socket, and retries the operation once. This provides transparent retry-on-transient-failure behavior.
- **Network cleanup on spawn failure**: If `QemuProcess::spawn()` fails after network setup (bridge/tap/veth already created), the code tears down those network resources before propagating the error. Guarantees no leaked network interfaces on spawn failure.
- **`try_lock` for log_stream/metrics_stream**: Both `log_stream` and `metrics_stream` use `try_lock()` on the instances Mutex instead of `.await lock()`. If the lock would block, they return an empty stream. Rationale: `try_lock()` is the only synchronous way to get `RunningInstance` without converting the method to async (which would break trait signature consistency). In practice it never fails because other methods hold the lock only for short synchronous sections. Empty stream = subscribe on next call.
- **`kill_on_drop(true)`**: `tokio::process::Child` is configured with `kill_on_drop(true)`. If the Rust handle is dropped (e.g., panic), the QEMU process is killed — prevents orphaned QEMU processes.
- **Log file failure is non-fatal**: If the log file cannot be opened (no permissions, bad path), it's not fatal — a warning is logged and `log_file_path` is set to `None`. The daemon still gets log lines via `tracing`/`subscribe_logs` independently.

### `metrics` — Host Metrics Collection

Collects resource metrics from `/proc` for the QEMU process. No QMP needed.

**Public API:**

| Function | Signature | Description |
|----------|-----------|-------------|
| `read_metrics_sample` | `fn(pid: u32) -> Option<ResourceMetrics>` | Single-point read of all metrics |
| `spawn_metrics_poller` | `fn(pid, ticks_per_sec, interval, sender) -> JoinHandle<()>` | Background task that reads metrics every `interval` and sends via `broadcast::Sender` |
| `ticks_per_second` | `fn() -> u64` | Reads `sysconf(_SC_CLK_TCK)` for CPU% calculation |

**Data sources:**
- **CPU%**: `/proc/<pid>/stat` fields utime (index 11) + stime (index 12), delta-based: `delta(utime+stime) / delta(uptime) / num_cpus * 100`
- **RAM**: `/proc/<pid>/status` VmRSS field
- **Disk I/O**: `/proc/<pid>/io` read_bytes/write_bytes
- **Net I/O**: `/proc/<pid>/net/dev` rx/tx bytes delta

**Polling interval**: 1 second (`DEFAULT_POLL_INTERVAL`).

### GPU metrics — in `andler-firmware`

GPU metrics (AMD/NVIDIA/Intel sysfs + NVML) live in `services/andler-firmware/src/metrics/` (`gpu_amd.rs`/`gpu_nvidia.rs`/`gpu_intel.rs`), alongside GPU vendor *detection* (`detect/gpu.rs`) — both read the same sysfs paths/vendor tools, so detection and monitoring belong in one crate. See that crate's README for the vendor details (sysfs paths, NVML/nvidia-smi output format, the Intel `rc6_residency_ms` load-delta calculation, etc.).

**Integration**: `spawn_metrics_poller` in `metrics.rs` calls `andler_firmware::metrics::read_gpu_metrics()` and merges the result into the base metrics sample every tick via `andler_firmware::metrics::merge_gpu_metrics()`. Single merged `ResourceMetrics` message per tick — no separate GPU channel.

## Tests

- **`cmdline`** (~30 tests): All argument blocks tested independently against reference configuration. Includes edge cases: `Passthrough` panic, `None` display engine, clipboard disabled, size suffixes.
- **`qmp`**: JSON parsing of QMP responses (`QmpReply`, `VmStatus`, `QueryStatusReturn`, `SnapshotInfo`, `QueryJobInfo`), plus `UnixStream::pair`-based fake-QMP-peer tests covering the real `snapshot-save`/`-load`/`-delete` wire schema (`devices`+`vmstate`), `wait_job_completion`'s `"concluded"`+`error` semantics, `job-dismiss`, and async-event skipping during polling.
- **`backend`** (~20 tests): `name_returns_qemu`, `Passthrough` validation, unknown handle handling, `VmStatus → InstanceState` mapping, empty `metrics_stream`/`log_stream`.
- **`process`**: `SpawnFailed` via missing binary, `drain_to_tracing` line publishing, subscriber tolerance, multiple subscribers fan-out.
- **`metrics`**: CPU stat parsing, CPU% computation, I/O rates, RSS parsing, net_dev parsing.
- GPU metrics tests (AMD/NVIDIA/Intel detection, NVIDIA output parsing, the Intel `rc6_residency_ms` ABI regression test, `intel_gpu_load_from_delta`/`compute_intel_gpu_load`, merge behavior) live in `services/andler-firmware` — see that crate's tests.

### With `/dev/kvm` and `qemu-system-x86_64` (integration tests, `#[ignore]`)

- `spawn_then_status_then_stop_round_trip`
- `spawn_then_pause_then_resume_round_trip`
- `log_stream_receives_real_process_output`
- `connect_then_pause_then_resume_round_trip` (QMP)
- `spawn_then_is_alive_then_terminate`
- `force_kill_stops_unresponsive_process`

All marked `#[ignore]` with reason — run separately in `integration-test` Docker target.

## Known Limitations

- **`status()` without live QMP**: If QMP socket isn't ready (just spawned, or connection dropped), degrades to "process alive, exact status unknown" — reflected in `detail`, not silently assumed `Running`.
- **No QMP event queue**: `qmp.rs` doesn't distinguish asynchronous events from command responses. Not a problem for current scope (stop/cont/query-status don't generate client-relevant events), but will be a limitation when event subscription is added.
- **Snapshot with virtio-sound**: `snapshot-save` with vmstate is blocked by QEMU when `virtio-sound-pci` is attached ("State blocked by non-migratable device") — a snapshot of an instance with `VirtioSound` audio fails with this error. This is an upstream QEMU constraint; see `docs/ARCHITECTURE.md` (Snapshots).
- **Hardcoded snapshot device name**: `drive-disk0`. Will need parameterization if multi-disk support is added.
- **Snapshot timeout**: Configurable per-instance via `DiskConfig::snapshot_timeout_secs` (default 30s). For very large snapshots (hundreds of GiB), this may need per-operation tuning.

## What Is NOT Implemented Here

- `RenderBackend::Passthrough` (VFIO GPU passthrough) — `gpu_display_args` panics, but `QemuBackend::spawn` checks `RenderBackend::is_implemented()` first and returns `BackendError::InvalidConfig`, never reaching `gpu_display_args` in normal flow.
