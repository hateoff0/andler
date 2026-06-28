# Architecture

ANDLER (**ANDLER** = *Android Linux Emulator & Runtime*) is a Rust monorepo for running and managing virtual machines (Android and Linux guests) on Linux/KVM. It follows a daemon + thin CLI architecture, with QEMU as the current hypervisor backend and gRPC as the communication protocol.

## Design Principles

1. **Domain-driven separation**: Pure domain types in `andler-core` with no infrastructure dependencies. All I/O (QEMU, filesystem, network, database) lives in separate service/backend crates.
2. **Backend abstraction**: The `HypervisorBackend` trait defines a hypervisor-agnostic interface. Adding a new hypervisor means implementing this trait — no changes to daemon, CLI, or RPC.
3. **Explicit state machine**: Instance lifecycle is governed by a strict FSM with named states and validated transitions. No implicit state changes.
4. **Persistence is optional**: The daemon works with or without SQLite persistence. In-memory state is always the source of truth for the current session. Store errors are logged but don't fail operations.
5. **No global state**: Each crate has clear responsibilities. `andler-core` knows nothing about QEMU, gRPC, or SQL. `andler-qemu` knows nothing about the daemon or CLI.
6. **User-mode by default**: All paths under `~/.local/share/andler/`. No root/sudo required for normal operation.

## Crate Dependency Graph

```
andler-core          (no workspace dependencies — bottom layer)
    ↑
    ├── andler-qemu  (depends on: core)
    ├── andler-vmm   (depends on: core)
    ├── andler-disk  (depends on: core types only via error, mostly standalone)
    ├── andler-net   (depends on: core)
    ├── andler-store (depends on: core)
    └── andler-rpc   (depends on: core)
          ↑
    andler-daemon    (depends on: core, qemu, disk, store, rpc)
          ↑
    andler-cli       (depends on: rpc only — thin client)
```

## Crates in Detail

### `core/andler-core` — Domain Model

The foundation crate. Defines all public types that other crates depend on.

**Key types:**
- `HypervisorBackend` trait: The contract for all hypervisor implementations (13 methods)
- `InstanceConfig`: 9-section configuration struct (disk, cpu, memory, display, gpu, network, audio, input, firmware)
- `InstanceState` / `InstanceEvent`: FSM with 7 states and 7 events
- `ResourceMetrics`: All-Optional metrics struct (CPU, RAM, disk I/O, net I/O, GPU)
- `CloneMode`: `Linked` | `FullStandalone` | `SharedBase`
- `AndroidProfile`: Android version + root mode + app store configuration
- `BackendError` / `FsmError`: Domain error types

**22 unit tests**, fully testable without QEMU or `/dev/kvm`.

**Must not depend on any other workspace crate.**

### `backends/andler-qemu` — QEMU Backend

Implements `HypervisorBackend` for QEMU via process management, QMP communication, and `/proc`-based metrics.

**Key components:**
- `cmdline.rs`: Pure function translating `InstanceConfig` to QEMU CLI arguments (10 argument blocks, each tested against `scripts/start.sh`)
- `process.rs`: `QemuProcess` — spawn, terminate, force-kill, log/metrics broadcast channels
- `qmp.rs`: `QmpClient` — QMP protocol over unix socket (handshake, pause/resume/status, snapshot job API)
- `backend.rs`: `QemuBackend` — ties everything together, manages `RunningInstance` registry
- `metrics.rs`: Background poller reading `/proc/<pid>/stat`, `/proc/<pid>/status`, `/sys/block/*/stat`, `/proc/<net/dev`
- `gpu_metrics.rs`: AMD sysfs reader for VRAM + GPU load

**50 tests** across 6 modules (unit + integration).

### `backends/andler-vmm` — Cloud Hypervisor (Stub)

Empty stub. Returns `NotImplemented` for all methods. Reserved for future `rust-vmm` / Cloud Hypervisor integration.

### `services/andler-disk` — Disk Operations

Wrapper around `qemu-img` for disk creation/cloning/resizing, plus offline Magisk provisioning.

**Key components:**
- `qcow2.rs`: 7 async functions wrapping `qemu-img` CLI
- `overlay.rs`: Android-specific overlay disk creation + factory reset
- `clone.rs`: 3 clone modes (linked, full-standalone, shared-base)
- `magisk.rs`: Offline Magisk provisioning via `qemu-nbd` with RAII guards

**19 unit tests** + 8 integration tests (`#[ignore]`).

### `services/andler-net` — Networking (Stub)

Contains only `NetworkConfig`/`NetworkMode` types. No real network setup logic yet — will handle bridge creation, nftables rules when implemented.

### `services/andler-store` — SQLite Persistence

Two-table SQLite store with JSON columns.

**Schema:**
- `instances(id TEXT PRIMARY KEY, config_json TEXT, state_json TEXT)`
- `snapshots(id, instance_id, tag, description, created_at)` with `ON DELETE CASCADE`

**19 tests** using in-memory SQLite.

### `services/andler-rpc` — gRPC Protocol

Protobuf definitions and generated code via `tonic`/`prost`.

**19 RPCs** covering instance lifecycle, monitoring, snapshots, clone/export.
**27 conversion tests** for bidirectional proto↔domain type mapping.

### `daemon/` — Background Service

Orchestrates all operations. Holds backend registry, instance state, optional persistence.

**Key design:**
- `Daemon::new()` / `with_store()` / `restore()` — three construction paths
- Instance lifecycle via FSM transitions
- `InstanceDirGuard` RAII for cleanup on partial failure
- `DaemonService` — thin gRPC wrapper, one method per Daemon method
- Error mapping: `DaemonError` → gRPC status codes

**65+ unit tests** + **25 gRPC round-trip tests** (real TCP).

### `cli/` — Command-Line Interface

Thin gRPC client. Each subcommand = one gRPC request + print response.

**Key features:**
- Unified `create` command (TOML for LinuxVm, CLI flags for AndroidVm)
- Real-time metrics streaming with GPU columns (AMD)
- Snapshot CRUD
- Clone/export operations

**7 TOML parsing tests**.

## Data Flow

```
User
  ↓ (CLI command)
andler-cli (gRPC client)
  ↓ (protobuf over TCP)
andler-daemon (gRPC server)
  ↓ (trait methods)
andler-qemu (HypervisorBackend)
  ↓ (process + QMP)
QEMU process → VM
  ↓ (/proc + sysfs)
Metrics poller → broadcast → StreamResourceMetrics → CLI display
  ↓ (SQLite)
andler-store (state.db)
```

## Instance Lifecycle FSM

```
                    ┌─────────┐
                    │ Created │
                    └────┬────┘
                         │ Start
                         ▼
                    ┌─────────┐
                    │ Starting│
                    └────┬────┘
                         │ StartCompleted
                         ▼
              ┌──── ┌─────────┐ ────┐
         Pause│     │ Running │     │Stop
              │     └────┬────┘     │
              ▼          │          ▼
         ┌─────────┐     │     ┌─────────┐
         │ Paused  │     │     │Stopping │
         └────┬────┘     │     └────┬────┘
              │Resume    │          │StopCompleted
              └────►┌────┘          ▼
                    │          ┌─────────┐
                    │          │ Stopped │──(Start)──►Created
                    │          └─────────┘
                    │
              Fail(msg)
                    ▼
              ┌─────────┐
              │  Error  │ (terminal)
              └─────────┘
```

Any active state (Created/Starting/Running/Paused/Stopping) can transition to `Error` via `Fail(msg)`.

## Snapshot Mechanism

Uses QEMU's async job API (not filesystem-level snapshots):

1. **Create**: `snapshot-save` job → poll `query-jobs` until complete (configurable timeout per-instance, default 30s)
2. **Restore**: `snapshot-load` job → poll `query-jobs`
3. **Delete**: `snapshot-delete` job → poll `query-jobs`
4. **List**: `query-block` → extract snapshot metadata

Snapshot metadata (tag, description, created_at) stored in SQLite `snapshots` table with `ON DELETE CASCADE` from `instances`.

## Metrics Collection

All host-side metrics from `/proc` — no QMP communication needed for metrics:

| Metric | Source | Calculation |
|--------|--------|-------------|
| CPU% | `/proc/<pid>/stat` (utime+stime), `/proc/uptime` | `delta(utime+stime) / delta(uptime) / num_cpus * 100` |
| RAM | `/proc/<pid>/status` (VmRSS) | Direct read |
| Disk I/O | `/sys/block/<dev>/stat` | Delta-based bytes/sec |
| Net I/O | `/proc/<net/dev>` | Delta-based bytes/sec |
| VRAM Used | `/sys/class/drm/card*/device/mem_info_vram_used` | AMD sysfs only |
| VRAM Total | `/sys/class/drm/card*/device/mem_info_vram_total` | AMD sysfs only |
| GPU Load | `/sys/class/drm/card*/device/gpu_busy_percent` | AMD sysfs only |

Polling interval: 1 second. Broadcast via `tokio::sync::broadcast`.

## Magisk Provisioning

Offline root access provisioning via `qemu-nbd`:

1. Find free NBD device (`/sys/class/block/nbd*/size` == 0)
2. Connect overlay qcow2 via `qemu-nbd --connect`
3. Wait for partition devices (2s timeout)
4. Mount root partition rw
5. Copy Magisk binaries to `/data/adb/magisk/`
6. Create modules directory structure
7. Patch boot image via `magiskboot` (best-effort)
8. Unmount + disconnect (RAII cleanup on all error paths)

**Requires:** `nbd` kernel module, `qemu-nbd` binary.

## Future Directions

- **Network modes**: Bridge/Isolated in `andler-net` with nftables integration
- **GPU passthrough**: VFIO-based `RenderBackend::Passthrough`
- **NVIDIA/Intel GPU metrics**: Extend sysfs reader after VFIO works
- **Cloud Hypervisor backend**: `andler-vmm` with `rust-vmm` crates
- **GUI**: Tauri-based client (planned, not started)
- **Guest image pipelines**: Automated Android image builds with Waydroid
