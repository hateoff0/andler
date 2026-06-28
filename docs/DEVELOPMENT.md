# Development

## Prerequisites

### Required

- **Rust** (stable, via [rustup](https://rustup.rs/))
- **Docker + Docker Compose** (for reproducible builds and E2E tests)
- **Linux** with KVM support (`/dev/kvm` must exist, user in `kvm` group)
- **QEMU** with UEFI/OVMF support (`qemu-system-x86_64`)
- **`protobuf-compiler`** (`protoc`) — required for gRPC code generation

### For Magisk Provisioning

- **`qemu-nbd`** (from `qemu-utils` package)
- **`nbd` kernel module** (`sudo modprobe nbd`)
- **Magisk release files** (`magisk` + `magiskinit` binaries)

### For Integration/E2E Tests

- **`qemu-utils`** (`qemu-img` for disk operations)
- **`/dev/kvm`** access for real QEMU process tests

## Building

### Local Build

```bash
cargo build --workspace
cargo build --release
```

### Docker Build (Recommended for Reproducibility)

```bash
# Unit tests
docker compose -f docker/docker-compose.yml build --no-cache unit-test
docker compose -f docker/docker-compose.yml run --rm unit-test

# E2E smoke test
docker compose -f docker/docker-compose.yml build --no-cache e2e
docker compose -f docker/docker-compose.yml run --rm e2e
```

## Running

### Start the Daemon

```bash
./target/release/andlerd
```

The daemon listens on `127.0.0.1:50051` by default. Override with:

```bash
# Via environment variable
ANDLERD_ADDR=http://0.0.0.0:50051 ./target/release/andlerd

# Via command-line flag (for the CLI client)
./target/release/andler --daemon-addr http://192.168.1.100:50051 status my-instance
```

### Store Path

Default: `~/.local/share/andler/state.db`. Override with:

```bash
ANDLERD_STORE_PATH=/path/to/state.db ./target/release/andlerd
```

### Default Paths

All instance data lives under `~/.local/share/andler/`:

```
~/.local/share/andler/
├── state.db                    # SQLite state store
├── instances/
│   └── <uuid>/
│       ├── instance.toml       # Instance configuration
│       ├── disk.qcow2          # Instance disk (or overlay)
│       └── VARS.fd             # Per-instance OVMF vars copy
└── images/                     # Base images (future)
```

## Testing

### Unit Tests

```bash
cargo test --workspace
```

All crates have unit tests that run without QEMU or `/dev/kvm`. Tests in `andler-core` are pure domain logic. Tests in `andler-qemu` use mocked data or test specific parsing/validation logic.

### Integration Tests

Integration tests require `qemu-img` and/or `/dev/kvm`. They're marked `#[ignore]` in the source and run separately:

```bash
# Via Docker (recommended)
docker compose -f docker/docker-compose.yml run --rm unit-test -- --ignored

# Or directly (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored
```

### E2E Smoke Test

The `docker/e2e_smoke.sh` script runs a full lifecycle test:

1. Start `andlerd` as a background process
2. Create an Android instance
3. Start the instance
4. Stream metrics for 5 seconds
5. Stop the instance
6. Remove the instance with `--purge`
7. Verify clean shutdown

```bash
docker compose -f docker/docker-compose.yml run --rm e2e
```

### gRPC Round-Trip Tests

`daemon/src/grpc_roundtrip_test.rs` contains 25 tests that verify the full gRPC pipeline:

- Real TCP connections (ephemeral ports)
- Real protobuf serialization/deserialization
- Real tonic server/client
- No `qemu-img` or `/dev/kvm` required

Run with:

```bash
cargo test -p andler-daemon grpc_roundtrip
```

### Metrics Smoke Test

The Docker E2E script includes a metrics verification step that:

1. Creates an instance
2. Streams metrics for 5 seconds
3. Verifies the output contains CPU/RAM columns
4. Checks for GPU columns if AMD hardware is present

## Project Structure

```
andler/
├── core/                          # Domain types, backend trait
│   └── andler-core/
│       └── src/
│           ├── lib.rs             # Re-exports
│           ├── backend.rs         # HypervisorBackend trait, ResourceMetrics
│           ├── fsm.rs             # InstanceState, InstanceEvent
│           ├── clone.rs           # CloneMode
│           ├── android_profile.rs # AndroidProfile, AndroidVersion
│           ├── error.rs           # BackendError, FsmError
│           └── config/            # 9 config modules
│               ├── mod.rs
│               ├── instance.rs    # InstanceConfig, InstanceId
│               ├── cpu.rs
│               ├── memory.rs
│               ├── disk.rs        # DiskConfig, snapshot_timeout_secs
│               ├── display.rs
│               ├── gpu.rs         # RenderBackend, GpuConfig
│               ├── network.rs
│               ├── firmware.rs
│               ├── audio.rs
│               └── input.rs
│
├── backends/                      # Hypervisor implementations
│   ├── andler-qemu/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── cmdline.rs        # QEMU CLI argument builder
│   │       ├── process.rs        # QemuProcess (spawn, logs, metrics)
│   │       ├── qmp.rs            # QMP client (pause, resume, snapshots)
│   │       ├── backend.rs        # QemuBackend (HypervisorBackend impl)
│   │       ├── metrics.rs        # /proc-based metrics poller
│   │       └── gpu_metrics.rs    # AMD sysfs GPU metrics
│   │
│   └── andler-vmm/               # Stub — future Cloud Hypervisor
│
├── services/                      # Infrastructure services
│   ├── andler-disk/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── qcow2.rs          # qemu-img wrapper (7 functions)
│   │       ├── overlay.rs        # Android overlay disks
│   │       ├── clone.rs          # 3 clone modes
│   │       ├── magisk.rs         # Offline Magisk provisioning
│   │       └── error.rs          # DiskError
│   │
│   ├── andler-net/               # Stub — future networking
│   │
│   ├── andler-store/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── store.rs          # SQLite store (instances + snapshots)
│   │       └── error.rs          # StoreError
│   │
│   └── andler-rpc/
│       ├── proto/andler.proto    # gRPC definitions
│       ├── build.rs              # Proto compilation
│       └── src/
│           ├── lib.rs            # Re-exports
│           └── convert.rs        # Proto ↔ Domain conversions
│
├── daemon/
│   └── src/
│       ├── main.rs               # Entry point, tonic server setup
│       ├── daemon.rs             # Daemon struct, lifecycle, persistence
│       ├── service.rs            # DaemonService (gRPC wrapper)
│       └── grpc_roundtrip_test.rs # Integration tests
│
├── cli/
│   └── src/
│       ├── main.rs               # CLI commands, gRPC client calls
│       └── instance_file.rs      # TOML config parser
│
├── docker/
│   ├── Dockerfile.dev            # Build environment
│   ├── docker-compose.yml        # Unit-test + E2E targets
│   └── e2e_smoke.sh              # End-to-end smoke test
│
├── docs/                          # Project documentation
│   ├── ARCHITECTURE.md
│   ├── DEVELOPMENT.md
│   ├── API.md
│   ├── CHANGELOG.md
│   └── archive/                  # Historical/planned docs
│
└── scripts/
    └── start.sh                  # Reference QEMU launch script
```

## Common Development Tasks

### Adding a New Config Field

1. Add field to the appropriate config struct in `core/andler-core/src/config/`
2. Add `reference_default()` update if needed
3. Add proto field in `services/andler-rpc/proto/andler.proto`
4. Add conversion in `services/andler-rpc/src/convert.rs`
5. Add CLI flag in `cli/src/main.rs` if applicable
6. Add TOML field in `cli/src/instance_file.rs` if applicable
7. Add test for the new field

### Adding a New Hypervisor Backend

1. Create `backends/andler-newbackend/` crate
2. Add to `Cargo.toml` workspace members
3. Implement `HypervisorBackend` trait from `andler-core`
4. Register in `Daemon::new()` / `default_backends()` in `daemon/src/daemon.rs`
5. Add `BackendKind` variant if needed in `andler-core`
6. Write tests (unit + integration)

### Adding a New gRPC Method

1. Add RPC definition in `services/andler-rpc/proto/andler.proto`
2. Add request/response message types if new
3. Add conversion functions in `services/andler-rpc/src/convert.rs`
4. Add daemon method in `daemon/src/daemon.rs`
5. Add gRPC handler in `daemon/src/service.rs`
6. Add CLI command in `cli/src/main.rs`
7. Add tests (daemon unit test + gRPC round-trip test)

### Adding a New Disk Operation

1. Add function in `services/andler-disk/src/qcow2.rs` (or new module)
2. Add error variant in `services/andler-disk/src/error.rs` if needed
3. Expose through daemon if needed
4. Write unit tests (parsing/logic) + integration tests (`#[ignore]` for real qemu-img)

### Modifying the Metrics Pipeline

1. Edit `backends/andler-qemu/src/metrics.rs` for host metrics
2. Edit `backends/andler-qemu/src/gpu_metrics.rs` for GPU metrics
3. Update `ResourceMetrics` in `core/andler-core/src/backend.rs` if adding fields
4. Update proto `ResourceMetricsResponse` in `proto/andler.proto`
5. Update conversion in `services/andler-rpc/src/convert.rs`
6. Update CLI display format in `cli/src/main.rs`

### Running Specific Test Suites

```bash
# All unit tests
cargo test --workspace

# Specific crate
cargo test -p andler-core
cargo test -p andler-qemu
cargo test -p andler-daemon

# Specific test module
cargo test -p andler-daemon daemon::tests
cargo test -p andler-daemon grpc_roundtrip

# Ignored (integration) tests
cargo test -p andler-qemu -- --ignored
cargo test -p andler-disk -- --ignored

# With output
cargo test --workspace -- --nocapture
```

## Code Style

- **No comments unless asked** — code should be self-documenting
- **English only** — all code, docs, commit messages
- **Russian allowed** only in `docs/archive/` for historical context
- **Domain types in `andler-core`** — no infrastructure dependencies
- **`async_trait`** for `HypervisorBackend` — allows async methods in trait objects
- **`tokio`** for async runtime, `tonic` for gRPC, `rusqlite` for SQLite
- **`thiserror`** for error types, `tracing` for logging
- **`serde`** for JSON (config persistence), `toml` for instance files
