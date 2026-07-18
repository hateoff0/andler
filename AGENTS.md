# Repository Guidelines

## Project Overview

ANDLER (**ANDLER** = *Android Linux Emulator & Runtime*) is a Rust monorepo for creating and managing QEMU virtual machines on Linux/KVM. The core value: **full 3D GPU acceleration** for Linux and Android guests via Venus (Vulkan), VirGL (OpenGL), and VFIO passthrough. Architecture: daemon + thin CLI, gRPC communication, QEMU hypervisor backend.

## Architecture & Data Flow

```
User → andler-cli (gRPC client) → andler-daemon (gRPC server) → andler-qemu (HypervisorBackend) → QEMU process → VM
                                                              ↓
                                                         andler-store (SQLite)
```

**Key design principles:**
- **Domain-driven separation**: Pure domain types in `andler-core` — no I/O dependencies
- **Backend abstraction**: `HypervisorBackend` trait defines hypervisor-agnostic interface (13 methods). New backend = implement this trait
- **Explicit FSM**: Instance lifecycle governed by strict state machine (7 states, 7 events). No implicit transitions
- **Persistence optional**: Daemon works with or without SQLite. In-memory is source of truth for current session
- **No global state**: Each crate has clear responsibilities

### Crate Dependency Graph

```
andler-core          (no workspace dependencies — bottom layer)
    ↑
    ├── andler-qemu     (depends on: core)
    ├── andler-vmm      (depends on: core) [stub]
    ├── andler-disk     (depends on: core types only)
    ├── andler-net      (depends on: core)
    ├── andler-store    (depends on: core)
    ├── andler-firmware (no workspace deps — standalone)
    └── andler-rpc      (depends on: core)
          ↑
    andler-daemon    (depends on: core, qemu, firmware, disk, store, rpc)
          ↑
    andler-cli       (depends on: rpc only — thin client)
```

### Instance Lifecycle FSM

```
Created → Starting → Running ⇄ Paused
                     ↓
                  Stopping → Stopped → (Start) → Starting
                     ↓
                  Error → (Start) → Starting
```

Any active state can transition to `Error` via `Fail(msg)`. `Stopped`/`Error` accept `Start` to restart.

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `core/andler-core/` | Domain types, `HypervisorBackend` trait, config structs, FSM, error types. **No workspace dependencies** |
| `backends/andler-qemu/` | QEMU backend: process management, QMP protocol, cmdline builder, `/proc`-based metrics |
| `backends/andler-vmm/` | Cloud Hypervisor stub (all methods return `NotImplemented`) |
| `services/andler-disk/` | `qemu-img` wrapper: disk creation/cloning/resizing, guest tools offline provisioning |
| `services/andler-net/` | Bridge/isolated network modes via `iproute2` |
| `services/andler-firmware/` | Hardware auto-detection, GPU metrics (NVIDIA/AMD/Intel), firmware discovery |
| `services/andler-store/` | SQLite persistence (two tables: `instances`, `snapshots`) |
| `services/andler-rpc/` | Protobuf definitions, gRPC generated code, proto↔domain conversions |
| `daemon/` | Background service: orchestrates backends, FSM transitions, gRPC server |
| `cli/` | Thin gRPC client: one subcommand = one gRPC request + print response |
| `docker/` | Build environment, compose targets, E2E smoke test |
| `docs/` | Architecture, development guide, API reference, changelog |
| `scripts/` | systemd service unit, installation script |

## Development Commands

### Build

```bash
# Local build
cargo build --workspace
cargo build --release

# Docker build (recommended for reproducibility)
docker compose -f docker/docker-compose.yml build --no-cache unit-test
```

### Run

```bash
# Start daemon (listens on 127.0.0.1:50051 by default)
./target/release/andlerd

# Override listen address
ANDLERD_LISTEN_ADDR=0.0.0.0:50051 ./target/release/andlerd

# Override store path
ANDLERD_STORE_PATH=/path/to/andlerd.db ./target/release/andlerd
```

### Test

```bash
# Unit tests (no QEMU/KVM required)
cargo test --workspace

# Docker unit tests
docker compose -f docker/docker-compose.yml run --rm unit-test

# Integration tests (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored

# gRPC round-trip tests (real TCP, no QEMU required)
cargo test -p andler-daemon grpc_roundtrip

# E2E smoke test
docker compose -f docker/docker-compose.yml run --rm e2e
```

### Lint & Format

```bash
# Rust toolchain: stable (see rust-toolchain.toml)
# Components: rustfmt, clippy

cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
```

## Code Conventions & Common Patterns

### Error Handling

- **`thiserror`** for all domain errors (`BackendError`, `FsmError`, `DaemonError`, `StoreError`, `DiskError`, `ConvertError`)
- `BackendError::NotImplemented` for unimplemented backend methods (never `todo!()`/`unimplemented!()`)
- `DaemonError` has 23 variants → maps to gRPC status codes
- `ConvertError` in `andler-rpc` for proto↔domain conversion failures

### Async Patterns

- **`tokio`** runtime with `rt-multi-thread`, `macros`, `process`, `fs`, `net`, `sync`, `time`, `io-util`, `signal`
- **`async_trait`** for `HypervisorBackend` trait (13 async methods)
- `rusqlite` calls wrapped in `tokio::task::spawn_blocking` (blocking API)
- `tokio::sync::RwLock` for `Daemon.instances` (concurrent reads, rare writes)
- `tokio::sync::broadcast` for metrics streaming

### Serialization

- **`serde`** with `derive` for domain types (TOML, JSON)
- **`prost`/`tonic`** for gRPC protobuf (no serde on proto types)
- Proto↔domain conversions: explicit `From`/`TryFrom` impls, never auto-derived
- `UNSPECIFIED` proto values → sensible fallbacks (e.g., `NatBackend::Slirp`, `CdromBus::Auto`)

### Naming & Formatting

- **Rust 2021 edition**, stable toolchain
- Module-level doc comments (`//!`) in Russian (project convention)
- Function-level docs in Russian or English
- Struct fields: `snake_case`, types: `PascalCase`
- Proto messages mirror domain types 1:1 by field
- CLI enums use `clap::ValueEnum` with `#[derive(Clone, Copy, PartialEq, Eq)]`

### Configuration Patterns

- **9-section `InstanceConfig`**: cpu, memory, disk, display, gpu, network, audio, input, firmware
- TOML instance files (auto-detect `LinuxVm` vs `AndroidVm` by content)
- Environment variables: `ANDLERD_*` prefix (`ANDLERD_LISTEN_ADDR`, `ANDLERD_STORE_PATH`, `ANDLERD_LOG_FORMAT`)
- CLI flags: `--daemon-addr`, `--kind`, `--file`, `--quick`

### Dependency Injection

- `Daemon::new()` / `with_store()` / `restore()` — three construction paths
- Backend registry: `HashMap<BackendKind, Arc<dyn HypervisorBackend>>`
- `default_backends()` function ensures consistent registry across constructors
- `Option<Store>` — persistence is opt-in, not mandatory

## Important Files

### Entry Points

- `daemon/src/main.rs` — daemon binary entry, tonic server setup, signal handling
- `cli/src/main.rs` — CLI dispatch, clap enums, gRPC client setup

### Core Domain

- `core/andler-core/src/backend.rs` — `HypervisorBackend` trait (13 methods), `ResourceMetrics`, `BackendHandle`
- `core/andler-core/src/fsm.rs` — `InstanceState` (7 states), `InstanceEvent` (7 events)
- `core/andler-core/src/config/` — 9 config modules (instance, cpu, memory, disk, display, gpu, network, audio, input)
- `core/andler-core/src/error.rs` — `BackendError`, `FsmError`
- `core/andler-core/src/paths.rs` — Unified path resolution (`runtime_dir()`, `db_path()`)

### Backend Implementation

- `backends/andler-qemu/src/cmdline.rs` — Pure function: `InstanceConfig` → QEMU CLI args
- `backends/andler-qemu/src/qmp.rs` — QMP protocol client (unix socket)
- `backends/andler-qemu/src/process.rs` — `QemuProcess` (spawn, logs, metrics)
- `backends/andler-qemu/src/metrics.rs` — `/proc`-based per-VM metrics poller

### RPC & Protocol

- `services/andler-rpc/proto/andler.proto` — 20 RPCs, all message/enum definitions
- `services/andler-rpc/src/convert.rs` — Bidirectional proto↔domain conversions (1000+ lines)

### Persistence

- `services/andler-store/src/store.rs` — SQLite store (instances + snapshots), `Arc<Mutex<Connection>>`

### Daemon Logic

- `daemon/src/daemon/mod.rs` — `Daemon` struct, constructors, backend registry
- `daemon/src/daemon/instance_ops.rs` — create/start/stop/pause/resume/remove
- `daemon/src/daemon/snapshot_ops.rs` — snapshot CRUD via QEMU job API
- `daemon/src/daemon/query_ops.rs` — status, list, metrics streaming
- `daemon/src/service.rs` — `DaemonService` (thin gRPC wrapper)
- `daemon/src/grpc_roundtrip_test.rs` — 24 integration tests (real TCP)

### CLI Commands

- `cli/src/create.rs` — Create instance (TOML or CLI flags)
- `cli/src/status.rs` — Status, List, Config, Logs, Metrics
- `cli/src/wizard/` — Interactive wizard with hardware auto-detection
- `cli/src/helpers.rs` — `parse_size`, `format_size`, `format_bytes`

### Config & Build

- `Cargo.toml` — Workspace definition, shared dependencies
- `rust-toolchain.toml` — Stable channel, rustfmt + clippy components
- `docker/docker-compose.yml` — Unit test, integration test, E2E targets
- `docker/Dockerfile.dev` — Multi-stage build environment
- `docker/e2e_smoke.sh` — Full lifecycle E2E test

## Runtime/Tooling Preferences

- **Language**: Rust 2021 edition, stable toolchain
- **Package manager**: Cargo (workspace)
- **Build**: `cargo build --workspace` or Docker
- **Protobuf compiler**: `protoc` (required for gRPC code generation)
- **Hypervisor**: QEMU (`qemu-system-x86_64`) with KVM
- **Target platform**: Linux only (requires `/dev/kvm`, `kvm` group)
- **User-mode**: All data under `~/.andler/` (no root required)
- **systemd**: User service unit in `scripts/andlerd.service`

### Key Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime (multi-thread) |
| `tonic` / `prost` | gRPC / protobuf |
| `clap` | CLI argument parsing |
| `thiserror` | Error type derivation |
| `async-trait` | Async methods in traits |
| `serde` | Serialization (TOML, JSON) |
| `rusqlite` | SQLite persistence |
| `tracing` | Structured logging |
| `nvml-wrapper` | NVIDIA GPU metrics |
| `uuid` | Instance IDs (v4) |

## Testing & QA

### Test Frameworks

- **Unit tests**: Built-in `#[cfg(test)]` modules in each crate
- **Integration tests**: `#[ignore]` attribute for tests requiring QEMU/KVM
- **E2E tests**: Shell script (`docker/e2e_smoke.sh`) with real daemon + CLI

### Test Counts

| Crate | Unit Tests | Integration Tests |
|-------|-----------|-------------------|
| `andler-core` | 43 | 0 |
| `andler-qemu` | varies | varies |
| `andler-disk` | 34 | 8 (ignored) |
| `andler-firmware` | 48 | 0 |
| `andler-store` | 19 | 0 |
| `andler-rpc` | 44 | 0 |
| `andler-daemon` | 73+ | 24 (gRPC round-trip) |
| `andler-cli` | 91 | 0 |

### Running Tests

```bash
# All unit tests
cargo test --workspace

# Specific crate
cargo test -p andler-core
cargo test -p andler-daemon grpc_roundtrip

# Integration tests (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored

# Docker tests (recommended)
docker compose -f docker/docker-compose.yml run --rm unit-test
docker compose -f docker/docker-compose.yml run --rm unit-test -- --ignored
docker compose -f docker/docker-compose.yml run --rm e2e
```

### Test Patterns

- Domain tests (`andler-core`): Pure logic, no I/O, no QEMU
- Backend tests (`andler-qemu`): Mocked data, parsing/validation, reference config comparison
- Store tests (`andler-store`): In-memory SQLite (`:memory:`)
- gRPC round-trip tests: Real TCP, real protobuf, real tonic server/client
- CLI tests: TOML parsing, helper functions, error formatting

### E2E Smoke Test

`docker/e2e_smoke.sh` runs full lifecycle:
1. Start `andlerd` background process
2. Create Android instance
3. Start instance
4. Stream metrics for 5 seconds
5. Stop instance
6. Remove instance with `--purge`
7. Verify clean shutdown

### Coverage Expectations

- All new features require unit tests
- Domain logic (`andler-core`) must be fully testable without QEMU
- gRPC changes require round-trip test additions
- Integration tests (`#[ignore]`) for QEMU-dependent paths
