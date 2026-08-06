# Development

## Prerequisites

### Required

- **Rust** (stable, via [rustup](https://rustup.rs/))
- **Docker + Docker Compose** (for reproducible builds and E2E tests)
- **Linux** with KVM support (`/dev/kvm` must exist, user in `kvm` group)
- **QEMU** with UEFI/OVMF support (`qemu-system-x86_64`)
- **`protobuf-compiler`** (`protoc`) — required for gRPC code generation

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

The Docker setup lives under `docker/dev/` (`Dockerfile.dev`, `docker-compose.yml`, `e2e_smoke.sh`); guest image pipelines live under `docker/images/`.

```bash
# Unit tests
docker compose -f docker/dev/docker-compose.yml build --no-cache unit-test
docker compose -f docker/dev/docker-compose.yml run --rm unit-test

# E2E smoke test
docker compose -f docker/dev/docker-compose.yml build --no-cache e2e
docker compose -f docker/dev/docker-compose.yml run --rm e2e
```

## Running

### Start the Daemon

```bash
./target/release/andlerd
```

The daemon listens on `127.0.0.1:50051` by default. Override with:

```bash
# Via environment variable (daemon listen address)
ANDLERD_LISTEN_ADDR=0.0.0.0:50051 ./target/release/andlerd

# Via command-line flag (for the CLI client)
./target/release/andler --daemon-addr http://192.168.1.100:50051 status my-instance
```

### Store Path

Default: `~/.andler/andlerd.db`. Override with:

```bash
ANDLERD_STORE_PATH=/path/to/andlerd.db ./target/release/andlerd
```

### Passwordless sudo for privileged operations

The daemon runs unprivileged; offline guest operations (offline `guest install`/`remove`, ARM translator switching, boot-mode switching) need root only for a fixed set of commands:

- `qemu-nbd` (connect/disconnect NBD devices)
- `mount` / `umount` (partitions, bind mounts, tmpfs)
- `chroot` (running package managers / writes inside the guest filesystem)
- `modprobe` (best-effort `nbd max_part=8` module autoload)

Configure with `andler doctor --fix` (writes `/etc/sudoers.d/andler`, validated with `visudo -c`), or manually. Without these rules, offline operations fail with an actionable message naming the missing sudoers entry.

### Default Paths

All instance data lives under `~/.andler/`:

```
~/.andler/
├── andlerd.db                    # SQLite state store
├── instances/
│   └── <id>/
│       ├── instance.toml       # Instance configuration
│       ├── disk.qcow2          # Instance disk (or overlay)
│       ├── VARS.fd             # Per-instance OVMF vars copy
│       ├── console.log         # QEMU serial console output
│       └── qemu.log            # QEMU stdout/stderr (log history for `andler logs`)
└── cache/
    ├── base-images/            # Android base images (*.manifest.json + qcow2)
    └── arm-translators/        # Downloaded ARM translators (libndk/libhoudini)
```

Runtime sockets live under `$XDG_RUNTIME_DIR` (default `/run/user/<uid>/`, 0700): the per-instance QMP socket (`<instance>/qmp.sock`) and the guest-agent chardev socket (`<instance>/qmp.qga.sock`).

## Testing

### Unit Tests

```bash
cargo test --workspace
```

All crates have unit tests that run without QEMU or `/dev/kvm`. Tests in `andler-core` are pure domain logic. Tests in `andler-qemu` use mocked data or test specific parsing/validation logic.

Test counts are not pinned anywhere in the docs — they drift as tests are added, and `cargo test --workspace` is the authoritative source. Approximate counts in READMEs are intentional.

### Integration Tests

Integration tests require `qemu-img` and/or `/dev/kvm`. They're marked `#[ignore]` in the source and run separately:

```bash
# Via Docker (recommended)
docker compose -f docker/dev/docker-compose.yml run --rm unit-test -- --ignored

# Or directly (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored
```

### E2E Smoke Test

The `docker/dev/e2e_smoke.sh` script runs a full lifecycle test:

1. Start `andlerd` as a background process
2. Create an Android instance from TOML (ID parsed from output)
3. Negative checks: `status` of a nonexistent instance fails with "not found", `disk create --size 0` is rejected, `create --disk-size-gib 0` is rejected by clap
4. Configure headless (`display_engine: None`), then really start QEMU under `/dev/kvm` → `Running`
5. Stream metrics (optional sample) and subscribe to logs — the log subscription catches the SIGTERM of QEMU
6. `stop --graceful` → `Stopped`; logs after stop are empty
7. Restart `andlerd` (persistence via SQLite restore) → `remove --purge` (disk.qcow2/VARS.fd deleted, foreign files kept)
8. Android clone cascade: `clone` linked (removing the source with a live linked clone fails with "live"), full-standalone, shared-base → `export` → count=4 → cascade remove

```bash
docker compose -f docker/dev/docker-compose.yml run --rm e2e
```

### gRPC Round-Trip Tests

`daemon/src/grpc_roundtrip_test.rs` contains a suite of tests that verify the full gRPC pipeline:

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
│           ├── base_image.rs      # Base image auto-discovery (~/.andler/cache/base-images)
│           ├── error.rs           # BackendError, FsmError
│           ├── paths.rs           # Unified path resolution (runtime_dir, current_uid, ensure_private_dir)
│           └── config/            # 9 config modules
│               ├── mod.rs
│               ├── instance.rs    # InstanceConfig, InstanceId, validate()
│               ├── cpu.rs
│               ├── memory.rs
│               ├── disk.rs        # DiskConfig, snapshot_timeout_secs
│               ├── display.rs
│               ├── gpu.rs         # RenderBackend, GpuConfig
│               ├── network.rs
│               ├── firmware.rs
│               ├── cdrom.rs       # CdromBus
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
│   │       └── metrics.rs        # /proc-based per-VM metrics poller (spawn_blocking)
│
├── services/                      # Infrastructure services
│   ├── andler-disk/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── qcow2.rs          # qemu-img wrapper (7 functions)
│   │       ├── overlay.rs        # Android overlay disks
│   │       ├── clone.rs          # 3 clone modes
│   │       ├── nbd.rs            # nbd device management (flock), nbd_status()
│   │       ├── guest_tools.rs    # offline guest provisioning (qemu-nbd + mount)
│   │       ├── arm_translator.rs # ARM translator package staging
│   │       ├── diskspace.rs      # free-space pre-check for snapshots
│   │       └── error.rs          # DiskError
│   │
│   ├── andler-net/               # Network configuration (bridge/isolated modes)
│
│   ├── andler-firmware/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── detect/           # Hardware auto-detection (GPU, OVMF, ARM, audio, passt)
│   │       └── metrics/          # GPU metrics (NVIDIA/AMD/Intel)
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
│       ├── daemon/
│       │   ├── mod.rs            # Daemon struct, constructors, persist helpers (~250 lines)
│       │   ├── error.rs          # DaemonError enum (29 variants)
│       │   ├── types.rs          # InstanceRecord, SnapshotRecord, InstanceDirGuard
│       │   ├── instance_ops.rs   # create/start/stop/pause/resume/remove + create_linux_instance + resolve_instance_id
│       │   ├── clone_ops.rs      # clone_instance, export, find_live_clones
│       │   ├── snapshot_ops.rs   # create/restore/delete/list snapshots
│       │   ├── query_ops.rs      # status, list, get_config, stream, update_instance_config
│       │   ├── health_ops.rs     # periodic VM health checks (ANDLERD_HEALTH_CHECK_INTERVAL_SECS)
│       │   └── tests/            # 12 test modules (incl. gRPC round-trip tests)
│       ├── service.rs            # DaemonService (gRPC wrapper)
│       └── grpc_roundtrip_test.rs # integration tests, real TCP
│
├── cli/
│   └── src/
│       ├── main.rs               # CLI dispatch + clap enums
│       ├── instance_file.rs      # TOML config parser
│       ├── create.rs             # Create command
│       ├── status.rs             # Status, List, Config, Logs, Metrics
│       ├── snapshot.rs           # Snapshot commands
│       ├── disk.rs               # Disk commands
│       ├── lifecycle.rs          # Start, Stop, Pause, Resume, Remove
│       ├── clone.rs              # Clone, Export
│       ├── guest.rs              # Guest package management + boot mode
│       ├── doctor.rs             # Environment diagnostics
│       ├── edit.rs               # `config edit` — editor-based config editing
│       ├── preview.rs            # `create --dry-run` resolution
│       ├── verify.rs             # `create --verify` checks
│       ├── wizard/               # Interactive wizard
│       │   ├── mod.rs            # Wizard entry point, handle_wizard()
│       │   ├── basic.rs          # BasicResult, ask_kind, ask_name, ask_iso, ask_disk
│       │   ├── advanced.rs       # AdvancedConfig, 16 ask_* functions
│       │   └── summary.rs        # SummaryAction, print_summary
│       └── helpers.rs            # parse_size, format_size, format_bytes, ensure_qcow2_extension
│
├── docker/
│   ├── dev/                      # Build environment + E2E
│   │   ├── Dockerfile.dev        # Build environment
│   │   ├── docker-compose.yml    # Unit-test + E2E targets
│   │   └── e2e_smoke.sh          # End-to-end smoke test
│   └── images/                   # Guest image pipelines (Waydroid, base images)
│
├── docs/                          # Project documentation
│   ├── ARCHITECTURE.md
│   ├── DEVELOPMENT.md
│   ├── API.md
│   ├── GRPC_API.md
│   ├── CHANGELOG.md
│   ├── ROADMAP.md
│   └── archive/                  # Historical/planned docs (may not exist)
│
└── scripts/
    ├── andlerd.service           # systemd user unit
    └── install.sh                # systemd installation script
```

## Common Development Tasks

### Adding a New Config Field

1. Add field to the appropriate config struct in `core/andler-core/src/config/`
2. Add `reference_default()` update if needed
3. Add proto field in `services/andler-rpc/proto/andler.proto`
4. Add conversion in `services/andler-rpc/src/convert.rs`
5. Add CLI flag in the appropriate module (`cli/src/create.rs`, `cli/src/lifecycle.rs`, etc.) if applicable
6. Add TOML field in `cli/src/instance_file.rs` if applicable
7. Add test for the new field

### Adding a New Hypervisor Backend

1. Create `backends/andler-newbackend/` crate
2. Add to `Cargo.toml` workspace members
3. Implement `HypervisorBackend` trait from `andler-core`
4. Register in `Daemon::new()` / `default_backends()` in `daemon/src/daemon/mod.rs`
5. Add `BackendKind` variant if needed in `andler-core`
6. Write tests (unit + integration)

### Adding a New gRPC Method

1. Add RPC definition in `services/andler-rpc/proto/andler.proto`
2. Add request/response message types if new
3. Add conversion functions in `services/andler-rpc/src/convert.rs`
4. Add daemon method in `daemon/src/daemon/mod.rs` (or appropriate ops file)
5. Add gRPC handler in `daemon/src/service.rs`
6. Add CLI command in the appropriate module (`cli/src/create.rs`, `cli/src/lifecycle.rs`, etc.)
7. Add tests (daemon unit test + gRPC round-trip test)

### Adding a New Disk Operation

### Adding a New Test

### Updating Dependencies

### Formatting and Linting

- **`cargo fmt`** for code formatting
- **`cargo clippy`** for linting

### Debugging Tips

- Use `tracing-subscriber` for structured logging
- Enable QEMU QMP logging with `-qmp unix:/tmp/qmp.sock,server,nowait`
- Use `tcpdump` or `Wireshark` for gRPC traffic analysis

### Release Process

1. Update `CHANGELOG.md`
2. Bump version in `Cargo.toml`
3. Build release artifacts
4. Tag and push

## Contributing

- Follow the Rust API guidelines
- Write tests for new functionality
- Keep backward compatibility in mind when changing APIs

## Getting Help

- Check existing issues and documentation
- Ask questions in the project repository

## License

GPL-3.0
