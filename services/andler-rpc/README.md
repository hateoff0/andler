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
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary | Requires stopped instance |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary | Requires stopped instance |
| `ListSnapshots` | `InstanceIdRequest` | `ListSnapshotsResponse` | Unary | |

## Key Proto Messages

### Instance Configuration

- **`CreateInstanceRequest`**: `name`, `iso_path`, `cpu`, `memory`, `disk`, `display`, `gpu`, `network`, `firmware`, `audio`, `input` — all 9 sub-configs as explicit fields. No intermediate resolution. Generates `InstanceId::new()` on the server side.
- **`CreateAndroidInstanceRequest`**: `name`, `profile` (AndroidProfile), `base_image_path`, `instances_root`, `overlay_size_bytes`, `ovmf_vars_template`, `magisk_dir` (optional, for Magisk provisioning).
- **`GetInstanceConfigResponse`**: Full `InstanceConfig` with `instance_id`, `name`, `kind`, `backend`, and all 9 sub-configs.

### Sub-Config Messages

- **`CpuConfig`**: `cores`, `sockets`, `threads`, `affinity` (repeated), `priority`.
- **`MemoryConfig`**: `size_bytes`, `ballooning`, `zram`, `ksm`.
- **`DiskConfig`**: `path`, `size_bytes`, `format`, `base_image`, `thin_provisioning`, `trim_on_shutdown`, `snapshot_timeout_secs` (optional).
- **`DisplayConfig`**: `resolution` (width/height), `dpi`, `fps_limit`, `display_engine`, `fullscreen`.
- **`GpuConfig`**: `render_backend` (oneof: Venus/VirtioGpu/VirGl/Cpu/Passthrough), `hostmem_bytes`, `blob`, `gl`.
- **`NetworkConfig`**: `mode` (oneof: Nat/Bridge{interface}/Isolated), `device_model`.
- **`FirmwareConfig`**: `ovmf_code_path`, `ovmf_vars_path`.
- **`AudioConfig`**: `backend`.
- **`InputConfig`**: `tablet_mode`, `hide_host_cursor`, `clipboard_enabled`.

### Instance Lifecycle

- **`InstanceStatusResponse`**: `state` (InstanceStateKind enum), `error_message`, `detail`.
- **`InstanceListEntry`**: `instance_id`, `name`, `state`.
- **`LogLineResponse`**: `source` (Stdout/Stderr), `line`.
- **`ResourceMetricsResponse`**: All 9 metric fields as optional (cpu_percent, memory_used_bytes, disk_read/write, net_rx/tx, vram_used/total, gpu_load_percent).

### Snapshots

- **`CreateSnapshotRequest`**: `instance_id`, `tag`, `description`.
- **`CreateSnapshotResponse`**: `snapshot_id`, `tag`, `created_at`.
- **`RestoreSnapshotRequest`**: `instance_id`, `tag`.
- **`DeleteSnapshotRequest`**: `instance_id`, `tag`.
- **`SnapshotEntry`**: `snapshot_id`, `tag`, `description`, `created_at`.

### Enums

`CpuPriority`, `DiskFormat`, `DisplayEngine`, `AndroidVersion`, `RootMode`, `AudioBackend`, `InstanceStateKind`, `BackendKind`, `CloneMode`, `LogStreamSource` — all with `UNSPECIFIED = 0` as default.

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

- `RenderBackend` and `NetworkMode` are `oneof` in proto (not C-style enums) because their domain equivalents carry data in variants (`Passthrough { gpu_pci_id }`, `Bridge { interface }`).
- `DisplayEngine::None` maps to proto `DisplayNone` (not `UNSPECIFIED`).
- `CreateInstanceRequest` → `InstanceConfig`: Generates new `InstanceId`, hardcodes `BackendKind::Qemu`.

## Build

`build.rs` compiles `.proto` via `tonic_build::compile_protos`. Requires system `protoc` (`protobuf-compiler` package — see `docker/Dockerfile.dev`).

`src/lib.rs`: `pub mod proto { tonic::include_proto!("andler"); }`

## Tests

27 tests in `convert::tests`:

- AndroidProfile round-trip through proto
- Unspecified enum rejection (AndroidVersion, RootMode, CpuPriority, DiskFormat, DisplayEngine, AudioBackend, CloneMode)
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
