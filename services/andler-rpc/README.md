# andler-rpc

gRPC protocol definition (`proto/andler.proto`) and generated server/client code (via `tonic`/`prost`), plus bidirectional conversions between generated types and `andler-core` domain types (`src/convert.rs`). Used by both `andler-daemon` (server) and `andler-cli` (client) — single source of truth for the wire protocol.

## Proto Service: `AndlerService`

| RPC | Request | Response | Streaming | Description |
|-----|---------|----------|-----------|-------------|
| `CreateInstance` | `CreateInstanceRequest` | `CreateInstanceResponse` | Unary | Create LinuxVm from explicit config (all 9 sections) |
| `CreateAndroidInstance` | `CreateAndroidInstanceRequest` | `CreateInstanceResponse` | Unary | Resolve AndroidProfile → create instance with overlay disk |
| `StartInstance` | `InstanceIdRequest` | `Empty` | Unary | |
| `StopInstance` | `StopInstanceRequest` | `Empty` | Unary | |
| `PauseInstance` | `InstanceIdRequest` | `Empty` | Unary | |
| `ResumeInstance` | `InstanceIdRequest` | `Empty` | Unary | |
| `GetInstanceStatus` | `InstanceIdRequest` | `InstanceStatusResponse` | Unary | |
| `ListInstances` | `Empty` | `ListInstancesResponse` | Unary | Returns only id/name/state, not full config |
| `RemoveInstance` | `RemoveInstanceRequest` | `Empty` | Unary | Rejects non-terminal states as FAILED_PRECONDITION |
| `GetInstanceConfig` | `InstanceIdRequest` | `GetInstanceConfigResponse` | Unary | Full config (all 9 sections + id/backend/kind) |
| `StreamInstanceLogs` | `InstanceIdRequest` | `stream LogLineResponse` | Server-streaming | Live-tail stdout/stderr; empty stream if no backend |
| `StreamResourceMetrics` | `InstanceIdRequest` | `stream ResourceMetricsResponse` | Server-streaming | Real-time metrics; empty stream if not found |
| `CloneInstance` | `CloneInstanceRequest` | `CreateInstanceResponse` | Unary | Supports LinuxVm + AndroidVm |
| `ExportInstanceDisk` | `ExportInstanceDiskRequest` | `ExportInstanceDiskResponse` | Unary | Exports disk as standalone file |
| `CreateSnapshot` | `CreateSnapshotRequest` | `CreateSnapshotResponse` | Unary | Requires Running/Paused instance |
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary | Requires Running/Paused instance |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary | Requires Running/Paused instance |
| `ListSnapshots` | `InstanceIdRequest` | `ListSnapshotsResponse` | Unary | |
| `UpdateInstanceConfig` | `UpdateInstanceConfigRequest` | `Empty` | Unary | Replace instance config (andler edit) |
| `InstallGuestAgent` | `InstallGuestAgentRequest` | `Empty` | Unary | Install package in guest (auto-fallback: online/offline) |
| `RemoveGuestAgent` | `RemoveGuestAgentRequest` | `Empty` | Unary | Remove package from guest (auto-fallback) |
| `ListGuestPackages` | `InstanceIdRequest` | `ListGuestPackagesResponse` | Unary | List known packages with status |
| `SwitchArmTranslator` | `SwitchArmTranslatorRequest` | `Empty` | Unary | Switch ARM translation backend for AndroidVm |
| `SetInstanceConfig` | `SetInstanceConfigRequest` | `Empty` | Unary | Set a single config key-value pair on an instance |
| `SwitchAndroidBootMode` | `SwitchAndroidBootModeRequest` | `Empty` | Unary | Switch Android boot mode for AndroidVm |
| `GetAndroidBootMode` | `InstanceIdRequest` | `GetAndroidBootModeResponse` | Unary | Get current Android boot mode |
| `ApplyGuestProfile` | `InstanceIdRequest` | `ApplyGuestProfileResponse` | Unary | Apply the guest-side work the instance's own config selects (ARM translator, SPICE clipboard agent); one classified outcome per selection |
| `ListRemoteBaseImages` | `ListRemoteBaseImagesRequest` | `ListRemoteBaseImagesResponse` | Unary | Published base images from the release catalog (GitHub releases), with local cache state; the daemon owns all HTTP, so CLI and GUI share this route |
| `DownloadBaseImage` | `DownloadBaseImageRequest` | `stream BaseImageDownloadProgress` | Server-streaming | Resolve one published build, verify it against the release manifest's sha256, install it into `~/.andler/cache/base-images/`; each message carries phase/asset/byte counters, the final one the installed path |

## Key Proto Messages

### Instance Configuration

- **`CreateInstanceRequest`**: `name`, `iso_path`, `cpu`, `memory`, `disk`, `display`, `gpu`, `network`, `firmware`, `audio`, `input`, `cdrom_bus` — all 10 sub-configs as explicit fields. No intermediate resolution. Generates `InstanceId::new()` on the server side.
- **`CreateAndroidInstanceRequest`**: `name`, `profile` (AndroidProfile), `base_image_path`, `instances_root`, `overlay_size_bytes`, `ovmf_vars_template`.
- **`GetInstanceConfigResponse`**: Full `InstanceConfig` with `instance_id`, `name`, `kind`, `backend`, and all 9 sub-configs.

### Sub-Config Messages

- **`CpuConfig`**: `cores`, `sockets`, `threads`, `affinity` (repeated), `priority`.
- **`MemoryConfig`**: `size_bytes`, `ballooning`, `zram`, `ksm`.
- **`DiskConfig`**: `path`, `size_bytes`, `format`, `base_image`, `thin_provisioning`, `trim_on_shutdown`, `snapshot_timeout_secs` (optional), `compact_on_shutdown`.
- **`DisplayConfig`**: `resolution` (width/height), `dpi`, `fps_limit`, `display_engine`, `fullscreen`.
- **`GpuConfig`**: `render_backend` (oneof: Venus/VirtioGpu/VirGl/Cpu/Passthrough), `hostmem_bytes`, `blob`, `gl`.
- **`NetworkConfig`**: `mode` (oneof: Nat/Bridge{interface}/Isolated), `device_model`, `nat_backend`.
- **`FirmwareConfig`**: `ovmf_code_path`, `ovmf_vars_path`.
- **`AudioConfig`**: `backend`, `device`.
- **`InputConfig`**: `pointer_mode`, `hide_host_cursor`, `clipboard_enabled`.
- **`GuestProfileEntry` / `GuestProfileStatus`**: `name` (`arm-translator`, `spice-vdagent`), `status` (`APPLIED`/`ALREADY_PRESENT`/`SKIPPED`/`FAILED`), `message` — one entry per selection, so a partial apply is visible instead of implied.
- **`RemoteBaseImageEntry`**: `id` (`android<major>-<variant>-<built_at>`), version/variant/`built_at`, `release_tag`, `download_bytes`, optional `installed_bytes`, `installed` + `installed_path`.
- **`BaseImageDownloadProgress` / `BaseImageDownloadPhase`**: phase (`RESOLVING`/`DOWNLOADING`/`VERIFYING`/`EXTRACTING`/`INSTALLING`/`DONE`), asset name + index/count, downloaded/total bytes, `message`, and `installed_path` on `DONE`.

### Instance Lifecycle

- **`InstanceStatusResponse`**: `state` (InstanceStateKind enum), `error_message`, `detail`.
- **`InstanceListEntry`**: `instance_id`, `name`, `state`.
- **`LogLineResponse`**: `source` (Stdout/Stderr), `line`.
- **`ResourceMetricsResponse`**: All 9 metric fields as optional (cpu_percent, memory_used_bytes, disk_read/write, net_rx/tx, vram_used/total, gpu_load_percent).

### Snapshots

- **`CreateSnapshotRequest`**: `instance_id`, `tag`, `description`, `timeout_secs` (optional).
- **`CreateSnapshotResponse`**: `snapshot_id`, `tag`, `created_at`.
- **`RestoreSnapshotRequest`**: `instance_id`, `tag`, `timeout_secs` (optional).
- **`DeleteSnapshotRequest`**: `instance_id`, `tag`, `timeout_secs` (optional).
- **`SnapshotEntry`**: `snapshot_id`, `tag`, `description`, `created_at`.

### Config Editing

- **`UpdateInstanceConfigRequest`**: Mirrors `GetInstanceConfigResponse` field-for-field (`instance_ref`, `name`, `kind`, `backend`, `cpu`, `memory`, `disk`, `display`, `gpu`, `network`, `firmware`, `audio`, `input`). Protects `id`, `kind`, and `disk.path` from modification.

### Guest Agent & Config

- **`InstallGuestAgentRequest`**: `instance_id`, `package`.
- **`RemoveGuestAgentRequest`**: `instance_id`, `package`.
- **`ListGuestPackagesResponse`**: `packages` (repeated `GuestPackageEntry`).
- **`GuestPackageEntry`**: `name`, `description`, `status`.
- **`SwitchArmTranslatorRequest`**: `instance_ref`, `translator` (ArmTranslator enum), `translator_dir`.
- **`SetInstanceConfigRequest`**: `instance_ref`, `key`, `value`.

### Enums

`CpuPriority`, `DiskFormat`, `DisplayEngine`, `AndroidVersion`, `AudioBackend`, `AudioDevice`, `CdromBus`, `NatBackend`, `PointerMode`, `ArmTranslator`, `InstanceStateKind`, `BackendKind`, `CloneMode`, `LogStreamSource` — all with `UNSPECIFIED = 0` as default.

## Conversions (`src/convert.rs`)

Bidirectional conversions between proto and domain types:

- **`TryFrom<proto::*>` for domain types**: Handles `Unspecified` enum variants as errors. Handles `Option` fields (empty repeated → `None`, empty string → `None`).
- **`From<domain::*>` for proto types**: Infallible conversions.
- **`From<ConvertError> for tonic::Status`**: Maps to `invalid_argument`.
- **`parse_instance_id(raw: &str) -> Result<InstanceId, ConvertError>`**: Validates UUID format.
- **`instance_state_to_proto(state: &InstanceState) -> (InstanceStateKind, String)`**: Converts FSM state to proto kind + detail message.
- **`From<ResourceMetrics> for proto::ResourceMetricsResponse`**: All 9 fields as optional.
- **`From<LogLine> for proto::LogLineResponse`**: Domain → proto (one direction only — client never sends log lines).

### Notable Conversion Details

- Guest-profile outcomes convert through `guest_profile_convert` (`GuestSelectionOutcome` → `GuestProfileEntry`), mirroring `provision_convert` for mutator ops. Base-image download types live in `andler-disk`, which this crate does not depend on, so their proto messages are built in the daemon (`daemon/image_ops.rs`) instead.

- `RenderBackend` and `NetworkMode` are `oneof` in proto (not C-style enums) because their domain equivalents carry data in variants (`Passthrough { gpu_pci_id }`, `Bridge { interface }`).
- `DisplayEngine::None` maps to proto `DisplayNone` (not `UNSPECIFIED`).
- `CreateInstanceRequest` → `InstanceConfig`: Generates new `InstanceId`, hardcodes `BackendKind::Qemu`.

## Build

`build.rs` compiles `.proto` via `tonic_build::compile_protos`. Requires system `protoc` (`protobuf-compiler` package — see `docker/e2e/Dockerfile`).

`src/lib.rs`: `pub mod proto { tonic::include_proto!("andler"); }`

## Tests

~45 tests in `convert::tests`:

- AndroidProfile round-trip through proto
- Unspecified enum rejection (AndroidVersion, CpuPriority, DiskFormat, DisplayEngine, AudioBackend, CloneMode)
- `parse_instance_id`: valid UUID, empty string, garbage
- `InstanceState::Error` carries message into detail tuple
- Full `CreateInstanceRequest` round-trip (all sub-configs)
- Edge cases: Passthrough GPU, Bridge network, overlay disk with base_image
- Missing required fields: cpu, display.resolution, render_backend.kind, network_mode.kind
- CPU affinity: empty → None, populated → round-trips
- `GetInstanceConfigResponse` preserves LinuxVm/AndroidVm kind, all 9 sub-configs
- `DisplayEngine::None` round-trip
- `LogLine` → `LogLineResponse` conversion
- `CloneMode` all variants round-trip
- `ResourceMetrics` all 9 fields, default (all None) → empty proto
