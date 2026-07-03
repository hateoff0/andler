# andler-daemon

The `andlerd` binary. Core of ANDLER — orchestrates all VM lifecycle operations, snapshot management, metrics streaming, and log tailing. Communicates with the CLI via gRPC.

## Architecture

```
main.rs  →  tonic::transport::Server
               ↓
service.rs  →  DaemonService (thin gRPC wrapper)
               ↓
daemon/
├── mod.rs          →  Daemon (backend registry + instance state + optional persistence)
├── error.rs        →  DaemonError enum (18 variants)
├── types.rs        →  InstanceRecord, SnapshotRecord, InstanceDirGuard, InstanceSummary
├── instance_ops.rs →  create, start, stop, pause, resume, remove
├── clone_ops.rs    →  clone_instance, export_instance_disk, find_live_clones
├── snapshot_ops.rs →  create/restore/delete/list snapshots (requires Running/Paused for QMP commands)
└── query_ops.rs    →  status, list_instances, get_instance_config, stream logs/metrics
               ↓
           HypervisorBackend trait → QemuBackend / VmmBackend
```

## Modules

### `main.rs` — Entry Point

- Sets up `tracing` subscriber
- Opens `Store` at `ANDLERD_STORE_PATH` (default: `~/.local/share/andler/state.db`)
- Calls `Daemon::restore(store)` to recover instances from previous sessions
- Starts tonic `Server` on `DEFAULT_LISTEN_ADDR` (`127.0.0.1:50051`)
- Listens on loopback only — no auth/TLS (see comment in source)

Overridable via:
- `ANDLERD_ADDR` env var or `--daemon-addr` CLI flag (for the CLI client)
- `ANDLERD_STORE_PATH` env var (for the daemon)

### `daemon/mod.rs` — Core Daemon Logic

**`Daemon`**: Holds the backend registry, instance state, and optional persistence.

| Field | Type | Description |
|-------|------|-------------|
| `backends` | `HashMap<BackendKind, Arc<dyn HypervisorBackend>>` | Registered backends |
| `instances` | `RwLock<HashMap<InstanceId, InstanceRecord>>` | Current instance state |
| `store` | `Option<Store>` | Optional SQLite persistence |

**Construction**:
- `Daemon::new()`: No persistence. For testing.
- `Daemon::with_store(store)`: With persistence, no restore. For fresh starts.
- `Daemon::restore(store)`: Restores from store. Non-terminal states become `Error` (backend handles don't survive restart).

**Instance Lifecycle Methods**:

| Method | Signature | FSM Transition |
|--------|-----------|----------------|
| `create_instance` | `async fn(InstanceConfig) -> Result<InstanceId, DaemonError>` | → Created |
| `create_android_instance` | `async fn(AndroidProfile, String, PathBuf, PathBuf, u64, PathBuf, Option<PathBuf>) -> Result<InstanceId, DaemonError>` | Creates overlay + registers |
| `start_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Created → Starting → Running |
| `stop_instance` | `async fn(InstanceId, bool) -> Result<(), DaemonError>` | Running/Paused → Stopping → Stopped |
| `pause_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Running → Paused |
| `resume_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Paused → Running |
| `status` | `async fn(InstanceId) -> Result<BackendStatus, BackendError>` | Queries backend |
| `remove_instance` | `async fn(InstanceId, bool) -> Result<(), DaemonError>` | Requires terminal state |
| `get_instance_config` | `async fn(InstanceId) -> Result<InstanceConfig, DaemonError>` | Read-only |
| `list_instances` | `async fn() -> Vec<InstanceSummary>` | Read-only |
| `stream_instance_logs` | `async fn(InstanceId) -> Result<BoxStream<'static, LogLine>, DaemonError>` | Empty stream if no backend |
| `stream_resource_metrics` | `async fn(InstanceId) -> Result<BoxStream<'static, ResourceMetrics>, DaemonError>` | Empty stream if not found |

**Snapshot Methods**:

| Method | Signature | Requirement |
|--------|-----------|-------------|
| `create_snapshot` | `async fn(InstanceId, String, Option<String>, Option<u64>) -> Result<SnapshotRecord, DaemonError>` | Running/Paused |
| `restore_snapshot` | `async fn(InstanceId, String, Option<u64>) -> Result<(), DaemonError>` | Running/Paused |
| `delete_snapshot` | `async fn(InstanceId, String, Option<u64>) -> Result<(), DaemonError>` | Running/Paused |
| `list_snapshots` | `async fn(InstanceId) -> Result<Vec<SnapshotRecord>, DaemonError>` | Any |

**Clone/Export Methods**:

| Method | Signature | Description |
|--------|-----------|-------------|
| `clone_instance` | `async fn(InstanceId, String, PathBuf, CloneMode) -> Result<InstanceId, DaemonError>` | Clone existing instance |
| `export_instance_disk` | `async fn(InstanceId, PathBuf) -> Result<(), DaemonError>` | Export disk as standalone file |

**Key Design Decisions**:
- `spawn`/`stop` failure transitions to `Error { message }`, not stuck in `Starting`/`Stopping`.
- `pause`/`resume` don't update `InstanceState` directly — real state synced via `status()` (QMP `query-status`).
- `remove_instance(purge=true)` refuses if live `Linked` clones exist (`find_live_clones`).
- `stream_instance_logs` returns empty stream (not error) if no backend — observing a non-existent process isn't an invalid request.

**Persistence**:
- `create_instance` saves to store immediately.
- `start_instance`/`stop_instance` save every intermediate FSM transition (`Starting`/`Stopping`).
- Store errors are logged (`tracing::error!`) but don't fail the operation — in-memory state is the source of truth for the current session.
- `pause_instance`/`resume_instance` don't persist separately — they don't change `InstanceState` directly.

**`InstanceDirGuard`** (RAII): Deletes `instance_dir` on drop if the creation sequence didn't complete. Used by `create_android_instance` and `clone_instance` for cleanup on partial failure.

### `daemon/error.rs` — Error Types

18 error variants mapping domain failures to gRPC status codes:
- `InstanceNotFound`, `NoBackendRegistered`, `InvalidTransition`
- `Backend`, `Disk`, `Firmware`, `Io`, `Restore`
- `InstanceNotRemovable`, `InstanceNotClonable`
- `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`
- `SnapshotNotFound`, `SnapshotAlreadyExists`
- `SnapshotOperationRequiresRunningInstance`
- `EmptyInstanceRef`, `InstanceRefNotFound`, `AmbiguousInstanceId`

### `daemon/types.rs` — Internal Types

- **`InstanceRecord`**: State + backend handle + config snapshot + filesystem paths
- **`SnapshotRecord`**: id, instance_id, tag, description, creation timestamp
- **`InstanceDirGuard`**: RAII cleanup — deletes instance directory on drop if creation sequence didn't complete
- **`InstanceSummary`**: Compact view for list operations

### `daemon/instance_ops.rs` — Instance Lifecycle

Handles: `create_instance`, `create_android_instance`, `start_instance`, `stop_instance`, `pause_instance`, `resume_instance`, `remove_instance`.

### `daemon/clone_ops.rs` — Clone & Export

Handles: `clone_instance` (Linked/FullStandalone/SharedBase modes), `export_instance_disk`, `find_live_clones`.

### `daemon/snapshot_ops.rs` — Snapshot Management

Handles: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots`.

### `daemon/query_ops.rs` — Status & Streaming

Handles: `status`, `stream_instance_logs`, `stream_resource_metrics`, `list_instances`, `get_instance_config`.

### `service.rs` — gRPC Service

**`DaemonService`**: Thin wrapper over `Daemon`. One `tonic::async_trait` method per `Daemon` method, no additional business logic.

**`From<DaemonError> for tonic::Status`**: Maps domain errors to gRPC status codes:
- `InstanceNotFound` → `NOT_FOUND`
- `NoBackendRegistered` → `UNIMPLEMENTED`
- `InvalidTransition` / `InstanceNotRemovable` / `InstanceNotClonable` → `FAILED_PRECONDITION`
- `SnapshotAlreadyExists` → `ALREADY_EXISTS`
- `SnapshotNotFound` → `NOT_FOUND`
- `SharedBaseNotSupportedForLinuxVm` / `InstanceHasLiveClones` → `FAILED_PRECONDITION`
- `SnapshotOperationRequiresRunningInstance` → `FAILED_PRECONDITION`
- `Backend(NotImplemented)` → `UNIMPLEMENTED`
- `Backend(HandleNotFound)` → `FAILED_PRECONDITION`
- `EmptyInstanceRef` / `AmbiguousInstanceId` → `INVALID_ARGUMENT`
- `InstanceRefNotFound` → `NOT_FOUND`
- Other (`Backend(other)`, `Disk`, `Io`, `Restore`, `Firmware`) → `INTERNAL`

### `grpc_roundtrip_test.rs` — Integration Tests

Real TCP gRPC round-trip tests (no `qemu-img`/`/dev/kvm` required). Uses ephemeral `127.0.0.1` ports.

**Categories**:
- **Unknown/malformed instance ID**: status/start/pause/resume/remove on unknown → `NotFound`; non-UUID → `InvalidArgument`
- **CreateInstance**: Full round-trip, missing required fields, Bridge network variant
- **ListInstances**: Empty, reflects created instances, multiple instances
- **RemoveInstance**: Happy path (create → start → fail → remove), unknown → `NotFound`
- **GetInstanceConfig**: Full config round-trip (all fields including oneof variants), unknown → `NotFound`
- **StreamInstanceLogs**: No backend → completes immediately, unknown → `NotFound`, malformed ID → `InvalidArgument`
- **CloneInstance**: Unknown source, LinuxVm + Linked (requires qemu-img), unspecified mode, malformed ID, LinuxVm + SharedBase → `FailedPrecondition`
- **ExportInstanceDisk**: Unknown source, LinuxVm (requires qemu-img)

## Tests

### `daemon::tests` (unit tests, no network)

68 tests across 9 modules:

| Module | Focus | Tests |
|--------|-------|-------|
| `common.rs` | Test infrastructure (TestTempDir, sample configs) | — |
| `create.rs` | Instance creation, Android overlay, TOML parsing | 5 |
| `start_stop.rs` | Start, pause, resume, stop lifecycle | 6 |
| `persistence.rs` | with_store, restore, without_store | 9 |
| `remove.rs` | Remove, purge, file cleanup, clone protection | 13 |
| `clone.rs` | Clone (3 modes), export, find_live_clones | 20 |
| `list_config.rs` | list_instances, get_instance_config | 7 |
| `status.rs` | Status queries, log/metrics streaming | 3 |
| `resolve_instance_id.rs` | Partial ID resolution, ambiguity detection | 5 |

- **Instance lifecycle**: create, start (with Passthrough validation), pause/resume before start, stop before start, double create overwrite
- **Android instance creation**: Profile resolution, overlay creation (`#[ignore]`), missing base image (verifies `InstanceDirGuard` cleanup), missing OVMF template (verifies cleanup)
- **Persistence**: with_store persists created instance, persists failed start as Error, without_store doesn't panic, restore from store (terminal states preserved, non-terminal → Error, empty store → no instances, restored daemon continues persisting)
- **List instances**: empty, one per created, reflects failed start state, after restore
- **Remove instance**: from Created/Error/Stopped, rejects all non-terminal states, removes from store, disappears from list, `purge: false` leaves files, `purge: true` deletes disk+OVMF+dir, keeps non-empty parent dir, never deletes base_image/ovmf_code, tolerates already-missing files, rejects when live linked clones exist, allows purge when clone is FullStandalone
- **Get instance config**: unknown → NotFound, returns full config, reflects overwrites
- **Clone/Export**: unknown → NotFound, LinuxVm + SharedBase → error, non-terminal source → error, Linked/FullStandalone/SharedBase modes (`#[ignore]`), clone of clone allowed, export creates standalone file without registering
- **find_live_clones**: empty when no references, finds linked clone, ignores shared base_image, rejects purge when live clone exists

### `grpc_roundtrip_test.rs` (integration, real TCP)

24 tests covering the full gRPC round-trip for all major operations.

## DaemonError Variants

| Variant | Fields | Description |
|---------|--------|-------------|
| `InstanceNotFound` | `InstanceId` | Not registered |
| `NoBackendRegistered` | `BackendKind` | Backend kind not in registry |
| `InvalidTransition` | `FsmError` | FSM transition not allowed |
| `Backend` | `BackendError` | Backend returned error |
| `Disk` | `DiskError` | Disk creation/provisioning error |
| `Firmware` | `String` | OVMF detection/provisioning error |
| `Io` | `path`, `source` | Filesystem error outside disk crate |
| `Restore` | `StoreError` | Failed to restore from store |
| `InstanceNotRemovable` | `InstanceId`, `InstanceState` | Non-terminal state |
| `InstanceNotClonable` | `InstanceId`, `InstanceState` | Non-terminal state |
| `SharedBaseNotSupportedForLinuxVm` | `InstanceId` | LinuxVm doesn't support SharedBase |
| `InstanceHasLiveClones` | `InstanceId`, `Vec<InstanceId>` | Can't purge with live linked clones |
| `SnapshotNotFound` | `instance_id`, `tag` | Snapshot doesn't exist |
| `SnapshotAlreadyExists` | `instance_id`, `tag` | Duplicate tag |
| `SnapshotOperationRequiresRunningInstance` | `InstanceId`, `InstanceState` | Create/restore/delete needs Running/Paused |
| `EmptyInstanceRef` | — | User passed empty string as instance reference |
| `InstanceRefNotFound` | `String` | Prefix matched zero instances |
| `AmbiguousInstanceId` | `prefix`, `candidates` | Prefix matched multiple instances |

## Requirements

- `/dev/kvm` access (user in `kvm` group) — no root, no `CAP_SYS_ADMIN`.
