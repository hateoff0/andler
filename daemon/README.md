# andler-daemon

The `andlerd` binary. Core of ANDLER — orchestrates all VM lifecycle operations, snapshot management, metrics streaming, and log tailing. Communicates with the CLI via gRPC.

## Architecture

```
main.rs  →  tonic::transport::Server
               ↓
service.rs  →  DaemonService (thin gRPC wrapper)
               ↓
firmware.rs →  OVMF auto-detection
               ↓
daemon/
├── mod.rs          →  Daemon (backend registry + instance state + optional persistence)
├── error.rs        →  DaemonError enum (27 variants)
├── types.rs        →  InstanceRecord, SnapshotRecord, InstanceDirGuard, InstanceSummary
├── instance_ops.rs →  create, create_linux_instance, start, stop, pause, resume, remove, resolve_instance_id
├── clone_ops.rs    →  clone_instance, export_instance_disk, find_live_clones
├── snapshot_ops.rs →  create/restore/delete/list snapshots (requires Running/Paused for QMP commands)
├── health_ops.rs   →  periodic crash detection for Running instances (Error transition, no auto-restart)
├── tests/          →  unit test modules (11 files, no network)
└── query_ops.rs    →  status, list_instances, get_instance_config, update_instance_config, stream logs/metrics
               ↓
           HypervisorBackend trait → QemuBackend / VmmBackend
```

## Modules

### `main.rs` — Entry Point

- Sets up `tracing` subscriber (verbosity: `-v` = debug, `-vv` = trace)
- Opens `Store` at `ANDLERD_STORE_PATH` (default: `~/.andler/andlerd.db`)
- Calls `Daemon::restore(store)` to recover instances from previous sessions
- Handles `SIGINT`/`SIGTERM` for graceful shutdown (stops all running instances)
- Starts tonic `Server` on `DEFAULT_LISTEN_ADDR` (`127.0.0.1:50051`)
- Listens on loopback only — no auth/TLS (see comment in source)

Overridable via:
- `ANDLERD_LISTEN_ADDR` env var (for the daemon's listen address)
- `ANDLERD_STORE_PATH` env var (for the database path)
- `ANDLERD_OVMF_CODE` env var (override OVMF_CODE path)
- `ANDLERD_OVMF_VARS` env var (override OVMF_VARS template path)
- CLI flags: `-v`/`--verbose` (debug), `-vv`/`--trace` (trace)

### `daemon/mod.rs` — Core Daemon Logic

**`Daemon`**: Holds the backend registry, instance state, and optional persistence.

| Field | Type | Description |
|-------|------|-------------|
| `backends` | `HashMap<BackendKind, Arc<dyn HypervisorBackend>>` | Registered backends |
| `instances` | `RwLock<HashMap<InstanceId, InstanceRecord>>` | Current instance state |
| `store` | `Option<Store>` | Optional SQLite persistence |
- **RwLock vs Mutex**: `instances` uses `tokio::sync::RwLock`, not `Mutex`, because `status()` (frequent call, e.g., GUI polling) only reads — no reason to block concurrent status reads during a single status check.
- **`Option<Store>` rationale**: `None` is not a temporary omission but a deliberate "in-memory only" mode. `Daemon::new()` (no persistence) is used by all existing tests — they test FSM/backend behavior and forcing them to carry sqlite adds unnecessary coupling.

**Construction**:
- `Daemon::new()`: No persistence. For testing.
- `Daemon::with_store(store)`: With persistence, no restore. For fresh starts. (`#[cfg(test)]` only)
- `Daemon::restore(store)`: Restores from store. Non-terminal states become `Error` (backend handles don't survive restart).

**Instance Lifecycle Methods**:

| Method | Signature | FSM Transition |
|--------|-----------|----------------|
| `create_instance` | `async fn(InstanceConfig) -> Result<InstanceId, DaemonError>` | → Created |
| `create_linux_instance` | `async fn(InstanceConfig, PathBuf, PathBuf) -> Result<InstanceId, DaemonError>` | Creates dir + disk + registers |
| `create_android_instance` | `async fn(AndroidProfile, String, PathBuf, PathBuf, u64, PathBuf) -> Result<InstanceId, DaemonError>` | Creates overlay + registers |
| `start_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Created → Starting → Running |
| `stop_instance` | `async fn(InstanceId, bool) -> Result<(), DaemonError>` | Running/Paused → Stopping → Stopped |
| `pause_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Running → Paused |
| `resume_instance` | `async fn(InstanceId) -> Result<(), DaemonError>` | Paused → Running |
| `status` | `async fn(InstanceId) -> Result<BackendStatus, DaemonError>` | Queries backend |
| `resolve_instance_id` | `fn(&str) -> Result<InstanceId, DaemonError>` | Partial ID resolution |
| `remove_instance` | `async fn(InstanceId, bool) -> Result<(), DaemonError>` | Requires terminal state |
| `get_instance_config` | `async fn(InstanceId) -> Result<InstanceConfig, DaemonError>` | Read-only |
| `update_instance_config` | `async fn(InstanceId, InstanceConfig) -> Result<(), DaemonError>` | Edit config (andler edit) |
| `list_instances` | `async fn() -> Vec<InstanceSummary>` | Read-only |
| `stream_instance_logs` | `async fn(InstanceId) -> Result<BoxStream<'static, LogLine>, DaemonError>` | Empty stream if no backend |
| `stream_resource_metrics` | `async fn(InstanceId) -> Result<BoxStream<'static, ResourceMetrics>, DaemonError>` | Empty stream if not found |
| `switch_arm_translator` | `async fn(InstanceId, ArmTranslator) -> Result<(), DaemonError>` | Switch ARM translator for Android, requires `is_disk_idle` |
| `set_instance_config` | `async fn(InstanceId, InstanceConfig) -> Result<(), DaemonError>` | Partial config update with persistence, requires `is_disk_idle` |

**Instance Configuration**:

**`compact_on_shutdown`**: When enabled in `InstanceConfig`, triggers background disk compaction after `stop_instance` completes. The instance transitions to `Stopped` immediately while compaction runs asynchronously. Subsequent `start_instance` calls wait for any pending compaction to complete. This reduces disk image size for instances with frequent write activity, at the cost of post-shutdown I/O latency.

**`is_disk_idle` Gating**: Operations that modify the instance's disk or configuration (`switch_arm_translator`, `set_instance_config`) require the disk to be idle — meaning no other process is holding the disk open (no running instance, no active NBD server, no in-flight compaction). This prevents data corruption from concurrent access. The daemon checks for competing processes by attempting to acquire an exclusive lock on the disk image before proceeding.

**Snapshot Methods**:

| Method | Signature | Requirement |
|--------|-----------|-------------|
| `create_snapshot` | `async fn(InstanceId, String, Option<String>, Option<u64>) -> Result<SnapshotRecord, DaemonError>` | Running/Paused |
| `restore_snapshot` | `async fn(InstanceId, String, Option<u64>) -> Result<(), DaemonError>` | Running/Paused |
| `delete_snapshot` | `async fn(InstanceId, String, Option<u64>) -> Result<(), DaemonError>` | Running/Paused |
| `list_snapshots` | `async fn(InstanceId) -> Result<Vec<SnapshotRecord>, DaemonError>` | Any |

**Snapshot Merge Semantics**: When restoring or deleting a snapshot, the system performs a merge operation that reconciles the snapshot's state with the current disk state:
- **Restore**: The current state is discarded and replaced with the snapshot's state. The previous state is automatically saved as a new snapshot with an auto-generated tag (`pre-restore-<timestamp>`) to prevent data loss.
- **Delete**: When deleting a snapshot that has children (subsequent snapshots depend on it), the merge recursively applies all descendant snapshots to the base, collapsing the chain into a single state. This is equivalent to repeatedly restoring the youngest descendant.
- **Chain Optimization**: After a merge operation, the daemon attempts to optimize the snapshot chain by coalescing adjacent snapshots where possible, reducing storage overhead.

**Guest Agent Methods**:

| Method | Signature | Description |
|--------|-----------|-------------|
| `install_guest_agent` | `async fn(InstanceId, String) -> Result<(), DaemonError>` | Install package in guest (auto-fallback: online/offline) |
| `remove_guest_agent` | `async fn(InstanceId, String) -> Result<(), DaemonError>` | Remove package from guest (auto-fallback) |
| `list_guest_packages` | `async fn(InstanceId) -> Result<Vec<(String, String, String)>, DaemonError>` | List known packages with status |

**Guest Agent Dual-Path**: The guest agent methods use a two-tier fallback strategy:
- **Online path (QMP)**: If the instance is `Running` and QMP socket is accessible, the agent communicates directly with the guest via QMP (`guest.exec`). This requires the guest to be running with QEMU's guest agent (`qemu-ga`) active.
- **Offline path (qemu-nbd)**: If QMP fails (instance not running or QEMU guest agent unavailable), the operation falls back to mounting the instance's disk image via `qemu-nbd` and performing the operation directly on the filesystem. This requires the disk to not be in use by another process.


**Clone/Export Methods**:

| Method | Signature | Description |
|--------|-----------|-------------|
| `clone_instance` | `async fn(InstanceId, String, PathBuf, CloneMode) -> Result<InstanceId, DaemonError>` | Clone existing instance |
| `export_instance_disk` | `async fn(InstanceId, PathBuf) -> Result<(), DaemonError>` | Export disk as standalone file |
| `find_live_clones` | `async fn(InstanceId) -> Result<Vec<InstanceId>, DaemonError>` | Find linked clones referencing this disk |

**Key Design Decisions**:
- `spawn`/`stop` failure transitions to `Error { message }`, not stuck in `Starting`/`Stopping`.
- `pause`/`resume` don't update `InstanceState` directly — real state synced via `status()` (QMP `query-status`).
- `remove_instance(purge=true)` refuses if live `Linked` clones exist (`find_live_clones`).
- `stream_instance_logs` returns empty stream (not error) if no backend — observing a non-existent process isn't an invalid request.

**Persistence**:
- `create_instance` saves to store immediately.
- `start_instance`/`stop_instance` save every intermediate FSM transition (`Starting`/`Stopping`).
- `update_instance_config` saves config changes to store and writes `instance.toml`.
- Store errors are logged (`tracing::error!`) but don't fail the operation — in-memory state is the source of truth for the current session.
- `pause_instance`/`resume_instance` don't persist separately — they don't change `InstanceState` directly.

**`InstanceDirGuard`** (RAII): Deletes `instance_dir` on drop if the creation sequence didn't complete. Used by `create_linux_instance`, `create_android_instance`, and `clone_instance` for cleanup on partial failure.

28 error variants mapping domain failures to gRPC status codes:
- `InstanceNotFound`, `NoBackendRegistered`, `InvalidTransition`
- `Backend`, `Disk`, `Firmware`, `Io`, `Restore`
- `InstanceNotRemovable`, `InstanceNotClonable`
- `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`
- `SnapshotNotFound`, `SnapshotAlreadyExists`
- `SnapshotOperationRequiresRunningInstance`, `SnapshotLimitExceeded`
- `EmptyInstanceRef`, `InstanceRefNotFound`, `AmbiguousInstanceId`
- `MalformedInstanceRef`
- `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged`
- `GuestAgentUnavailable`, `InvalidConfigKey`, `NotAndroid`, `InstanceMustBeStopped`
- `InstanceAlreadyStopped` — attempt to stop an already-stopped instance

**Detailed Error Semantics** (28 variants):

| Variant | gRPC Code | Rationale |
|---------|-----------|-----------|
| `InstanceNotFound` | `NOT_FOUND` | Instance not registered with this Daemon (different from `BackendError::HandleNotFound` which is about a specific backend handle) |
| `NoBackendRegistered` | `UNIMPLEMENTED` | Requested `BackendKind` not in registry (default registry has only `Qemu`) |
| `InvalidTransition` | `FAILED_PRECONDITION` | FSM transition not allowed from current state (see `andler_core::fsm`) |
| `Backend` | `INTERNAL` | Backend returned error during operation |
| `Disk` | `INTERNAL` | `andler-disk` error creating overlay for Android instance |
| `Firmware` | `INTERNAL` | `andler-firmware` error detecting OVMF or provisioning personal `OVMF_VARS` copy |
| `Io` | `INTERNAL` | Filesystem error outside `andler-disk` — instance dir creation, etc. |
| `Restore` | `INTERNAL` | `andler-store` error restoring instances from DB at startup |
| `InstanceNotRemovable` | `FAILED_PRECONDITION` | `remove_instance` called for non-terminal state — deleting a running instance is forbidden, not silently stopped first |
| `InstanceNotClonable` | `FAILED_PRECONDITION` | `clone`/`export` called for non-terminal state — copying/linking disk of live QEMU process is unsafe |
| `SharedBaseNotSupportedForLinuxVm` | `FAILED_PRECONDITION` | `CloneMode::SharedBase` for `LinuxVm` — Linux VMs have standalone disks, no shared base image |
| `InstanceHasLiveClones` | `FAILED_PRECONDITION` | `remove_instance(purge: true)` called while live `Linked` clones exist — purge would delete the backing file those clones depend on |
| `SnapshotNotFound` | `NOT_FOUND` | Snapshot with given tag not found for instance |
| `SnapshotAlreadyExists` | `ALREADY_EXISTS` | Snapshot with given tag already exists for instance |
| `SnapshotOperationRequiresRunningInstance` | `FAILED_PRECONDITION` | Create/restore/delete requires `Running`/`Paused` (performed via QMP) |
| `SnapshotLimitExceeded` | `FAILED_PRECONDITION` | Instance already has `MAX_SNAPSHOTS_PER_INSTANCE` — prevents unbounded internal snapshot growth |
| `EmptyInstanceRef` | `INVALID_ARGUMENT` | Empty string passed as instance reference (distinct from `InstanceRefNotFound`) |
| `MalformedInstanceRef` | `INVALID_ARGUMENT` | Reference string is neither valid UUID nor valid hex prefix |
| `InstanceRefNotFound` | `NOT_FOUND` | Prefix matched zero instances |
| `AmbiguousInstanceId` | `INVALID_ARGUMENT` | Prefix matched multiple instances — candidates listed in message |
| `ConfigIdMismatch` | `INVALID_ARGUMENT` | Config has different `InstanceId` than the one resolved from request |
| `ConfigKindChanged` | `INVALID_ARGUMENT` | Config changes guest kind (`LinuxVm` ↔ `AndroidVm`) — not supported, requires recreation |
| `ConfigDiskPathChanged` | `INVALID_ARGUMENT` | Config changes `disk.path` — must use `andler disk` commands to safely move/relink |
| `GuestAgentUnavailable` | `INTERNAL` | Guest agent binary unavailable or failed to start |
| `InvalidConfigKey` | `INVALID_ARGUMENT` | Invalid or unknown configuration key for `update_instance_config` |
| `NotAndroid` | `FAILED_PRECONDITION` | Operation requires Android instance (`switch_arm_translator`) |
| `InstanceAlreadyStopped` | `FAILED_PRECONDITION` | `stop_instance` called when instance is already in `Stopped` or `Error` state — nothing to stop |
| `InstanceMustBeStopped` | `FAILED_PRECONDITION` | Operation requires stopped instance (`set_instance_config`, `switch_arm_translator`) |

### `daemon/instance_ops.rs` — Instance Lifecycle

Handles: `create_instance`, `create_linux_instance`, `create_android_instance`, `start_instance`, `stop_instance`, `pause_instance`, `resume_instance`, `remove_instance`, `resolve_instance_id`.

**Pre-start validation**: `validate_instance_files()` checks that the disk and firmware files referenced by `InstanceConfig` exist on disk before calling `backend.spawn()`. Catches deleted/moved instance directories early — without this, `spawn()` would fork `qemu-system-x86_64` successfully (the OS-level exec succeeds regardless), report the instance as `Running`, and only reveal the real failure a health-check cycle later.

### `daemon/clone_ops.rs` — Clone & Export

Handles: `clone_instance` (Linked/FullStandalone/SharedBase modes), `export_instance_disk`, `find_live_clones`.

### `daemon/snapshot_ops.rs` — Snapshot Management

Handles: `create_snapshot`, `restore_snapshot`, `delete_snapshot`, `list_snapshots`.

### `daemon/health_ops.rs` — Crash Detection

Handles: `run_health_check_once` (spawned periodically from `main.rs`, `ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s). Polls every `Running` instance's real backend status; if the process has died outside `stop_instance`, transitions the FSM record to `Error` and persists it. Doesn't auto-restart the instance itself — `andler start <id>` works on it right after (see `fsm.rs`: `Stopped`/`Error` accept `Start`), this is a deliberate policy choice (silent auto-restart on top of a possibly-broken config risks masking a real failure behind a crash loop with no attempt limit/backoff), not a technical limitation. See the module doc comment for the full reasoning.

### `daemon/query_ops.rs` — Status & Streaming

Handles: `status`, `stream_instance_logs`, `stream_resource_metrics`, `list_instances`, `get_instance_config`, `update_instance_config`.

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
- `SnapshotLimitExceeded` → `FAILED_PRECONDITION`
- `Backend(NotImplemented)` → `UNIMPLEMENTED`
- `Backend(HandleNotFound)` → `FAILED_PRECONDITION`
- `EmptyInstanceRef` / `AmbiguousInstanceId` / `MalformedInstanceRef` → `INVALID_ARGUMENT`
- `ConfigIdMismatch` / `ConfigKindChanged` / `ConfigDiskPathChanged` → `INVALID_ARGUMENT`
- `InstanceRefNotFound` → `NOT_FOUND`
- `GuestAgentUnavailable` → `INTERNAL`
- `InvalidConfigKey` → `INVALID_ARGUMENT`
- `NotAndroid` → `FAILED_PRECONDITION`
- `InstanceMustBeStopped` → `FAILED_PRECONDITION`
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

73 tests across 10 modules:

| Module | Focus | Tests |
|--------|-------|-------|
| `common.rs` | Test infrastructure (TestTempDir, sample configs) | — |
| `create.rs` | Instance creation, Android overlay, TOML parsing | 6 |
| `start_stop.rs` | Start, pause, resume, stop lifecycle | 6 |
| `persistence.rs` | with_store, restore, without_store | 9 |
| `remove.rs` | Remove, purge, file cleanup, clone protection | 14 |
| `clone.rs` | Clone (3 modes), export, find_live_clones | 20 |
| `list_config.rs` | list_instances, get_instance_config | 7 |
| `status.rs` | Status queries, log/metrics streaming | 3 |
| `resolve_instance_id.rs` | Partial ID resolution, ambiguity detection | 5 |
| `health.rs`             | Health check unit tests | — |

- **Instance lifecycle**: create, start (with Passthrough validation), pause/resume before start, stop before start, double create overwrite
- **Android instance creation**: Profile resolution, overlay creation (`#[ignore]`), missing base image (verifies `InstanceDirGuard` cleanup), missing OVMF template (verifies cleanup)
- **Persistence**: with_store persists created instance, persists failed start as Error, without_store doesn't panic, restore from store (terminal states preserved, non-terminal → Error, empty store → no instances, restored daemon continues persisting)
- **List instances**: empty, one per created, reflects failed start state, after restore
- **Remove instance**: from Created/Error/Stopped, rejects all non-terminal states, removes from store, disappears from list, `purge: false` leaves files, `purge: true` deletes disk+OVMF+dir, keeps non-empty parent dir, never deletes base_image/ovmf_code, tolerates already-missing files, rejects when live linked clones exist, allows purge when clone is FullStandalone
- **Get instance config**: unknown → NotFound, returns full config, reflects overwrites
- **Clone/Export**: unknown → NotFound, LinuxVm + SharedBase → error, non-terminal source → error, Linked/FullStandalone/SharedBase modes (`#[ignore]`), clone of clone allowed, export creates standalone file without registering


### `grpc_roundtrip_test.rs` (integration, real TCP)

24 tests covering the full gRPC round-trip for all major operations.

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
| `SnapshotLimitExceeded` | `instance_id`, `current`, `limit` | Already has MAX_SNAPSHOTS_PER_INSTANCE |
| `EmptyInstanceRef` | — | User passed empty string as instance reference |
| `MalformedInstanceRef` | `String` | Not a valid UUID or hex prefix |
| `InstanceRefNotFound` | `String` | Prefix matched zero instances |
| `AmbiguousInstanceId` | `prefix`, `candidates` | Prefix matched multiple instances |
| `ConfigIdMismatch` | `expected`, `actual` | Config has a different id |
| `ConfigKindChanged` | `InstanceId` | Cannot change instance kind via edit |
| `ConfigDiskPathChanged` | `InstanceId` | Cannot change disk path via edit |
| `GuestAgentUnavailable` | `InstanceId` | Guest agent binary unavailable or failed to start |
| `InvalidConfigKey` | `String` | Invalid or unknown configuration key |
| `NotAndroid` | `InstanceId` | Operation requires Android instance |
| `InstanceMustBeStopped` | `InstanceId`, `InstanceState` | Operation requires stopped instance |

## Requirements

- `/dev/kvm` access (user in `kvm` group) — no root, no `CAP_SYS_ADMIN`.
