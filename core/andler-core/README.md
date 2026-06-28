# andler-core

Domain model for ANDLER: instance configuration, finite state machine, and hypervisor backend abstraction. This is the foundation crate that all other crates depend on.

## Modules

### `backend` — Hypervisor Backend Abstraction

Defines the `HypervisorBackend` trait — the contract that all hypervisor implementations (QEMU, future Cloud Hypervisor) must fulfill.

**`HypervisorBackend`** (async trait, `Send + Sync`):

| Method | Signature | Description |
|--------|-----------|-------------|
| `name` | `fn(&self) -> &'static str` | Machine-readable backend name |
| `supported_render_backends` | `fn(&self) -> &[RenderBackend]` | Which GPU/render modes this backend supports |
| `spawn` | `async fn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>` | Start a new VM instance |
| `pause` | `async fn(&self, handle: &BackendHandle) -> Result<(), BackendError>` | Pause a running instance |
| `resume` | `async fn(&self, handle: &BackendHandle) -> Result<(), BackendError>` | Resume a paused instance |
| `stop` | `async fn(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>` | Stop an instance (graceful = SIGTERM, else kill) |
| `status` | `async fn(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>` | Query current status |
| `snapshot` | `async fn(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>` | Create a snapshot (default: `NotImplemented`) |
| `snapshot_restore` | `async fn(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>` | Restore from snapshot (default: `NotImplemented`) |
| `snapshot_delete` | `async fn(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>` | Delete a snapshot (default: `NotImplemented`) |
| `snapshot_list` | `async fn(&self, handle: &BackendHandle) -> Result<Vec<SnapshotInfo>, BackendError>` | List snapshots (default: `NotImplemented`) |
| `metrics_stream` | `fn(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>` | Real-time resource metrics (CPU%, RAM, disk, net, GPU) |
| `log_stream` | `fn(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine>` | Live-tail stdout/stderr of the hypervisor process |

Any method not implemented by a specific backend must return `BackendError::NotImplemented` — never panic. Exception: `metrics_stream`/`log_stream` have signatures (`fn`, not `async fn`, no `Result`) that don't allow returning errors; "not implemented" or "no active stream" is expressed as an immediately-empty stream.

**`BackendHandle`**: Opaque identifier for a running instance (e.g., PID + QMP socket path for QEMU).

**`BackendStatus`**: Current state as seen by the backend, with optional human-readable detail string.

**`ResourceMetrics`**: All fields are `Option` — partial data is valid:

| Field | Type | Source |
|-------|------|--------|
| `cpu_percent` | `Option<f32>` | `/proc/<pid>/stat` delta-based |
| `memory_used_bytes` | `Option<u64>` | `/proc/<pid>/status` VmRSS |
| `disk_read_bytes_per_sec` | `Option<u64>` | `/sys/block/<dev>/stat` |
| `disk_write_bytes_per_sec` | `Option<u64>` | `/sys/block/<dev>/stat` |
| `net_rx_bytes_per_sec` | `Option<u64>` | `/proc/<net/dev>` delta |
| `net_tx_bytes_per_sec` | `Option<u64>` | `/proc/<net/dev>` delta |
| `vram_used_bytes` | `Option<u64>` | AMD sysfs `mem_info_vram_used` |
| `vram_total_bytes` | `Option<u64>` | AMD sysfs `mem_info_vram_total` |
| `gpu_load_percent` | `Option<f32>` | AMD sysfs `gpu_busy_percent` |

**`SnapshotInfo`**: `tag` (user-facing identifier), `id` (backend identifier), `created_at` (format is backend-specific).

**`LogLine`** / **`LogStreamSource`**: A single line from the hypervisor's stdout or stderr. Only raw process output — no structured FSM events from the daemon, no guest logs.

### `fsm` — Finite State Machine

Instance lifecycle states and transitions.

**`InstanceState`**:

```
Created → Starting → Running ⇄ Paused → Stopping → Stopped → Created
                                    ↘ Error (any active state + Fail)
```

| Variant | Description |
|---------|-------------|
| `Created` | Config saved, process never started |
| `Starting` | `spawn` called, awaiting confirmation |
| `Running` | VM is executing |
| `Paused` | Suspended via `pause` |
| `Stopping` | Stop requested, awaiting termination |
| `Stopped` | Process terminated, resources freed |
| `Error { message: String }` | Backend returned an error. Terminal state. |

**`InstanceEvent`**: `Start`, `StartCompleted`, `Pause`, `Resume`, `Stop`, `StopCompleted`, `Fail(String)`.

**`InstanceState::apply(self, event) -> Result<InstanceState, FsmError>`**: Pure function — applies an event to the current state and returns the new state. No side effects.

**`InstanceState::is_terminal(&self) -> bool`**: `true` for `Stopped` and `Error` — no outgoing transitions.

### `config/` — Instance Configuration

Nine configuration sections, each in its own file:

#### `config::instance` — Top-Level Config

- **`InstanceId`**: Newtype wrapper around `Uuid` (v4). Unique per instance.
- **`BackendKind`**: `Qemu` | `Vmm` (Vmm is registered but returns `NotImplemented`).
- **`InstanceKind`**: `LinuxVm { iso_path }` | `AndroidVm { android_profile }`.
- **`InstanceConfig`**: The full configuration struct combining all sections:

```rust
pub struct InstanceConfig {
    pub id: InstanceId,
    pub name: String,
    pub kind: InstanceKind,
    pub backend: BackendKind,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,
    pub disk: DiskConfig,
    pub display: DisplayConfig,
    pub gpu: GpuConfig,
    pub network: NetworkConfig,
    pub firmware: FirmwareConfig,
    pub audio: AudioConfig,
    pub input: InputConfig,
}
```

Each sub-config has a `reference_default()` method that produces sensible defaults matching the reference `scripts/start.sh`.

#### `config::cpu`

- `CpuConfig`: `cores` (total vCPUs), `sockets`, `threads`, `affinity: Option<Vec<usize>>` (CPU pinning), `priority: CpuPriority` (`Low` | `Normal` | `High`).
- Default: 4 cores, 1 socket, 1 thread, no affinity, Normal priority.

#### `config::memory`

- `MemoryConfig`: `size_bytes`, `ballooning` (virtio-balloon), `zram` (guest ZRAM), `ksm` (host KSM sharing).
- Default: 8 GiB, no ballooning, no zram, KSM enabled.

#### `config::disk`

- `DiskConfig`: `path`, `size_bytes`, `format` (`Qcow2` | `Raw` | `Vdi`), `base_image: Option<PathBuf>` (backing file for overlays), `thin_provisioning`, `trim_on_shutdown`, `snapshot_timeout_secs: Option<u64>` (per-instance snapshot job timeout, default 30s).
- `reference_default(path)`: 40 GiB qcow2, no backing, thin, discard.
- `overlay(path, base_image, size_bytes)`: Creates an overlay config with backing file.

#### `config::gpu`

- `RenderBackend`: `Venus` | `VirtioGpu` | `VirGl` | `Cpu` | `Passthrough { gpu_pci_id }`. Only `Passthrough` is not implemented (`is_implemented()` returns false).
- `GpuConfig`: `render_backend`, `hostmem_bytes`, `blob` (required for Venus), `gl` (OpenGL on display).
- Default: Venus, 4096 MiB hostmem, blob+gl on.

#### `config::display`

- `DisplayEngine`: `Sdl` | `Spice` | `Dbus` | `None` (headless, `-display none`).
- `DisplayConfig`: `resolution` (width/height), `dpi`, `fps_limit` (0 = unlimited), `display_engine`, `fullscreen`.
- Default: 1920x1080, 96 DPI, no limit, SDL, no fullscreen.

#### `config::network`

- `NetworkMode`: `Nat` | `Bridge { interface }` | `Isolated`.
- `NetworkConfig`: `mode`, `device_model` (virtio-net-pci).
- Default: NAT, virtio-net-pci.

#### `config::firmware`

- `FirmwareConfig`: `ovmf_code_path` (shared read-only OVMF_CODE), `ovmf_vars_path` (per-instance copy).
- Default: OVMF_CODE at `/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd`.

#### `config::audio`

- `AudioBackend`: `Pipewire` | `Pulseaudio` | `None`.
- `AudioConfig`: `backend`.
- Default: PipeWire.

#### `config::input`

- `InputConfig`: `tablet_mode` (virtio-tablet-pci, absolute positioning), `hide_host_cursor`, `clipboard_enabled` (qemu-vdagent).
- Default: all true.

### `android_profile` — Android Instance Profiles

- **`AndroidVersion`**: `Android11` | `Android13`.
- **`RootMode`**: `None` (default) | `Magisk`.
- **`AndroidProfile`**: `android_version`, `gapps`, `microg`, `libndk`, `root`.
  - `cache_key()`: Version/cache key string for base image lookup.
  - `resolve(...)`: Pure function — resolves profile into a full `InstanceConfig` with overlay disk. Does not download or create files.

### `clone` — Clone Modes

- **`CloneMode`**: Three ways to create a new disk from an existing instance's disk:
  - `Linked`: Overlay with `backing_file` pointing at source disk. Cheap/fast but creates dependency — source cannot be purged while linked clones exist.
  - `FullStandalone`: Flattens entire backing chain into an independent file. Expensive (full copy) but fully independent.
  - `SharedBase`: Byte-copy of source disk file. Physically independent from source (survives source purge), but remains thin relative to the shared base image. Compromise between the other two.

### `error` — Error Types

- **`BackendError`**: `NotImplemented`, `HandleNotFound(String)`, `Io(String)`, `InvalidConfig { backend, reason }`.
- **`FsmError`**: `InvalidTransition { from, event }`.

## Tests

22 unit tests across 13 test modules. Fully testable without QEMU or `/dev/kvm` — this is the whole point of extracting the domain into a separate crate. If a test in `andler-core` requires a real QEMU process, it's in the wrong crate.

| Module | Tests |
|--------|-------|
| `clone` | `clone_mode_variants_are_distinct` |
| `fsm` | `happy_path_start_pause_resume_stop`, `cannot_resume_from_running`, `cannot_pause_from_created`, `fail_is_reachable_from_every_active_state`, `terminal_states_have_no_outgoing_transitions` |
| `android_profile` | `cache_key_does_not_depend_on_root_mode`, `cache_key_differs_on_gapps`, `resolve_produces_overlay_disk_pointing_at_base_image` |
| `config::instance` | `instance_id_is_unique`, `config_round_trips_through_serde_json` |
| `config::cpu` | `reference_default_matches_start_sh` |
| `config::memory` | `reference_default_matches_start_sh` |
| `config::gpu` | `passthrough_is_not_implemented`, `venus_and_friends_are_implemented`, `reference_default_matches_start_sh` |
| `config::disk` | `reference_default_matches_start_sh`, `overlay_points_at_base_image` |
| `config::display` | `reference_default_uses_sdl`, `none_display_engine_round_trips_through_serde_json` |
| `config::network` | `reference_default_matches_start_sh` |
| `config::firmware` | `reference_default_matches_start_sh_code_path` |
| `config::audio` | `reference_default_matches_start_sh` |
| `config::input` | `reference_default_matches_start_sh` |

## What Must NOT Live Here

- No direct QEMU invocations or filesystem operations — that's `andler-qemu` and `andler-disk`.
- No gRPC/protocol serialization — that's `andler-rpc`.
- No SQL — that's `andler-store`.

`andler-core` must not depend on any other workspace crate — it is the bottom layer. Any attempt to add a dependency "upward" (on `andler-qemu`, `andler-rpc`, etc.) means the code is in the wrong place.
