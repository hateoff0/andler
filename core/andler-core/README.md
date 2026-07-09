# andler-core

Domain model for ANDLER: instance configuration, finite state machine, and hypervisor backend abstraction. This is the foundation crate that all other crates depend on.

## Modules

### `paths` — Unified Path Resolution

Single source of truth for all filesystem paths. Reads `ANDLER_HOME` env var, falls back to `~/.local/share/andler/`.

| Function | Returns |
|----------|---------|
| `andler_home()` | Root data directory |
| `instances_root()` | `<home>/instances/` |
| `base_images_dir()` | `<home>/base-images` |
| `ovmf_cache_dir()` | `<home>/ovmf/` |
| `venus_cache_dir()` | `<home>/venus-cache` |
| `db_path()` | `<home>/andlerd.db` |
| `runtime_dir()` | `$XDG_RUNTIME_DIR` or `/run/user/<uid>` or `std::env/temp_dir()` |
| `current_uid()` | Raw `getuid(2)` FFI call |
| `ensure_private_dir(dir)` | Create dir with 0700 permissions (async) |
| `ensure_private_dir_sync(dir)` | Create dir with 0700 permissions (sync) |

### `backend` — Hypervisor Backend Abstraction

Defines the `HypervisorBackend` trait — the contract that all hypervisor implementations (QEMU, future Cloud Hypervisor) must fulfill.

**`HypervisorBackend`** (async trait, `Send + Sync`):

| Method | Signature | Description |
|--------|-----------|-------------|
| `name` | `fn(&self) -> &'static str` | Machine-readable backend name |
| `supported_render_backends` | `fn(&self) -> &[RenderBackend]` | Which GPU/render modes this backend supports |
| `spawn` | `async fn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>` | Start a new VM instance |
| `pause` | `async fn(&self, handle: &BackendHandle) -> Result<(), BackendError>` | Pause a running instance |
| `resume` | `async fn(&self, handle: &BackendHandle) -> Result<(), BackendError>` | Resume paused instance |
| `stop` | `async fn(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>` | Stop an instance (graceful = ACPI shutdown via QMP; false = forceful process termination) |
| `status` | `async fn(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>` | Query current status |
| `snapshot` | `async fn(&self, handle: &BackendHandle, tag: &str, timeout: Option<Duration>) -> Result<(), BackendError>` | Create a snapshot (default: `NotImplemented`) |
| `snapshot_restore` | `async fn(&self, handle: &BackendHandle, tag: &str, timeout: Option<Duration>) -> Result<(), BackendError>` | Restore from snapshot (default: `NotImplemented`) |
| `snapshot_delete` | `async fn(&self, handle: &BackendHandle, tag: &str, timeout: Option<Duration>) -> Result<(), BackendError>` | Delete a snapshot (default: `NotImplemented`) |
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
| `disk_read_bytes_per_sec` | `Option<u64>` | `/proc/<pid>/io` |
| `disk_write_bytes_per_sec` | `Option<u64>` | `/proc/<pid>/io` |
| `net_rx_bytes_per_sec` | `Option<u64>` | `/proc/<net/dev>` delta |
| `net_tx_bytes_per_sec` | `Option<u64>` | `/proc/<net/dev>` delta |
| `vram_used_bytes` | `Option<u64>` | AMD sysfs / NVIDIA NVML + nvidia-smi / Intel sysfs |
| `vram_total_bytes` | `Option<u64>` | AMD sysfs / NVIDIA NVML + nvidia-smi / Intel sysfs |
| `gpu_load_percent` | `Option<f32>` | AMD sysfs / NVIDIA NVML + nvidia-smi / Intel busyiffies delta |

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
- **`InstanceKind`**: `LinuxVm { iso_path, cdrom_bus }` | `AndroidVm { android_profile }`.
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

Each sub-config has a `reference_default()` method that produces sensible defaults matching the original reference script (now removed).

#### `config::cpu`

- `CpuConfig`: `cores` (total vCPUs), `sockets`, `threads`, `affinity: Option<Vec<usize>>` (CPU pinning), `priority: CpuPriority` (`Low` | `Normal` | `High`).
- Default: 4 cores, 1 socket, 1 thread, no affinity, Normal priority.

#### `config::memory`

- `MemoryConfig`: `size_bytes`, `ballooning` (virtio-balloon), `zram` (guest ZRAM), `ksm` (host KSM sharing).
- Default: 8 GiB, no ballooning, no zram, KSM enabled.

#### `config::disk`

- `DiskConfig`: `path`, `size_bytes`, `format` (`Qcow2` | `Raw` | `Vdi`), `base_image: Option<PathBuf>` (backing file for overlays), `thin_provisioning`, `trim_on_shutdown`, `snapshot_timeout_secs: Option<u64>` (per-instance snapshot job timeout, default 30s), `compact_on_shutdown: bool` (auto-compact after stop, default false).
- `reference_default(path)`: 256 GiB qcow2, no backing, thin, discard.
- `overlay(path, base_image, size_bytes)`: Creates an overlay config with backing file.

#### `config::gpu`

- `RenderBackend`: `Venus` | `VirtioGpu` | `VirGl` | `Cpu` | `Passthrough { gpu_pci_id }`. Only `Passthrough` is not implemented (`is_implemented()` returns false).
- `GpuConfig`: `render_backend`, `hostmem_bytes`, `blob` (required for Venus), `gl` (OpenGL on display).
- Default: Venus, 4096 MiB hostmem, blob+gl on.

#### `config::display`

- `DisplayEngine`: `Sdl` | `Gtk` | `Spice` | `Dbus` | `None` (headless, `-display none`).
- `DisplayConfig`: `resolution` (width/height), `dpi`, `fps_limit` (0 = unlimited), `display_engine`, `fullscreen`.
- Default: 1920x1080, 96 DPI, no limit, SDL, no fullscreen.

#### `config::network`

- `NetworkMode`: `Nat` | `Bridge { interface }` | `Isolated`.
- `NetworkConfig`: `mode`, `device_model` (virtio-net-pci), `nat_backend: NatBackend` (`Slirp` | `Passt`).
- Default: NAT, virtio-net-pci, Slirp.

#### `config::firmware`

- `FirmwareConfig`: `ovmf_code_path` (shared read-only OVMF_CODE), `ovmf_vars_path` (per-instance copy).
- Default: OVMF_CODE at `/usr/share/edk2/x64/OVMF_CODE.4m.fd`.

#### `config::audio`

- `AudioBackend`: `Pipewire` | `Pulseaudio` | `None`.
- `AudioDevice`: `VirtioSound` | `Ich9Hda`.
- `AudioConfig`: `backend`, `device`.
- Default: PipeWire, VirtioSound.

#### `config::input`

- `PointerMode`: `Tablet` (absolute, virtio-tablet-pci) | `Mouse` (relative, virtio-mouse-pci).
- `InputConfig`: `pointer_mode`, `hide_host_cursor`, `clipboard_enabled` (qemu-vdagent).
- Default: Tablet, hide cursor, clipboard on.

#### `config::cdrom`

- `CdromBus`: `VirtioScsi` | `Ide`.
- `recommended_for_iso_filename(path)`: Auto-selects VirtioScsi for known Linux distros, Ide for Windows/unknown.
- Default: Ide (safe fallback).

### `android_profile` — Android Instance Profiles

- **`AndroidVersion`**: `Android11` | `Android13`.
- **`RootMode`**: `None` (default) | `Magisk`.
- **`ArmTranslator`**: `None` (default) | `Libndk` (recommended for AMD) | `Libhoudini` (recommended for Intel).
- **`AndroidProfile`**: `android_version`, `gapps`, `microg`, `arm_translator`, `root`.
  - `cache_key()`: Version/cache key string for base image lookup.
  - `resolve(...)`: Pure function — resolves profile into a full `InstanceConfig` with overlay disk. Does not download or create files.

### `clone` — Clone Modes

- **`CloneMode`**: Three ways to create a new disk from an existing instance's disk:
  - `Linked`: Overlay with `backing_file` pointing at source disk. Cheap/fast but creates dependency — source cannot be purged while linked clones exist.
  - `FullStandalone`: Flattens entire backing chain into an independent file. Expensive (full copy) but fully independent.
  - `SharedBase`: Byte-copy of source disk file. Physically independent from source (survives source purge), but remains thin relative to the shared base image. Compromise between the other two.

### `error` — Error Types

- **`BackendError`**: `NotImplemented { backend, operation }`, `HandleNotFound(String)`, `Io(String)`, `InvalidConfig { backend, reason }`.
- **`FsmError`**: `InvalidTransition { from, event }`.

## Tests

~41 unit tests across 15 test modules. Fully testable without QEMU or `/dev/kvm` — this is the whole point of extracting the domain into a separate crate. If a test in `andler-core` requires a real QEMU process, it's in the wrong crate.

| Module | Tests |
|--------|-------|
| `paths` | `andler_home_respects_env_override`, `andler_home_ignores_empty_env_override`, `derived_paths_are_nested_under_andler_home`, `runtime_dir_respects_xdg_runtime_dir_env`, `runtime_dir_ignores_empty_xdg_runtime_dir_env`, `ensure_private_dir_sync_creates_dir_with_0700`, `ensure_private_dir_async_creates_dir_with_0700` |
| `clone` | `clone_mode_variants_are_distinct` |
| `fsm` | `happy_path_start_pause_resume_stop`, `cannot_resume_from_running`, `cannot_pause_from_created`, `fail_is_reachable_from_every_active_state`, `terminal_states_have_no_outgoing_transitions` |
| `android_profile` | `cache_key_differs_on_arm_translator`, `cache_key_does_not_depend_on_root_mode`, `cache_key_differs_on_gapps`, `resolve_produces_overlay_disk_pointing_at_base_image` |
| `config::instance` | `instance_id_is_unique`, `config_round_trips_through_serde_json` |
| `config::cpu` | `reference_default_matches_start_sh` |
| `config::memory` | `reference_default_matches_start_sh` |
| `config::gpu` | `passthrough_is_not_implemented`, `venus_and_friends_are_implemented`, `reference_default_matches_start_sh` |
| `config::disk` | `reference_default_is_256_gib_thin_provisioned_qcow2`, `overlay_points_at_base_image` |
| `config::display` | `reference_default_uses_sdl`, `none_display_engine_round_trips_through_serde_json` |
| `config::network` | `reference_default_matches_start_sh`, `nat_backend_deserializes_with_default_when_missing` |
| `config::firmware` | `reference_default_has_expected_structure`, `with_code_sets_both_paths` |
| `config::audio` | `reference_default_backend_matches_start_sh`, `reference_default_device_is_virtio_sound_not_start_sh`, `device_field_deserializes_with_default_when_missing` |
| `config::input` | `reference_default_matches_start_sh`, `pointer_mode_deserializes_with_default_when_missing` |
| `config::cdrom` | `recommends_virtio_scsi_for_known_distros`, `recommends_ide_for_unknown_or_windows_iso`, `recommends_ide_when_path_has_no_filename`, `default_is_ide_not_virtio` |

## What Must NOT Live Here

- No direct QEMU invocations or filesystem operations — that's `andler-qemu` and `andler-disk`.
- No gRPC/protocol serialization — that's `andler-rpc`.
- No SQL — that's `andler-store`.

`andler-core` must not depend on any other workspace crate — it is the bottom layer. Any attempt to add a dependency "upward" (on `andler-qemu`, `andler-rpc`, etc.) means the code is in the wrong place.
