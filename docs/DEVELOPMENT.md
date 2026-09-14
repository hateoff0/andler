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

The containerized test harness lives under `docker/e2e/` (`Dockerfile`, `compose.yaml`, `e2e.sh`, `tests/`); guest image pipelines live under `docker/images/`. BuildKit cache mounts make rebuilds incremental — no `--no-cache` needed (see `docker/e2e/README.md` for details and troubleshooting).

```bash
# Unit tests
docker compose -f docker/e2e/compose.yaml build unit-test
docker compose -f docker/e2e/compose.yaml run --rm unit-test

# E2E suite
docker compose -f docker/e2e/compose.yaml build e2e
docker compose -f docker/e2e/compose.yaml run --rm e2e
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

### Zero-root guest operations

The daemon runs unprivileged and performs no privileged operations: offline guest work (`guest install`/`remove`, ARM-translator switching, boot-mode switching) runs through the libguestfs appliance, which mounts the disk in its own QEMU VM and runs the guest's own tools there as root. There is no privileged helper binary and no sudoers rule.

Prerequisites (`andler doctor`): guestfs-tools (`guestfish`) and KVM.

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
    ├── base-images/            # Android base images (flat or android<N>-<variant>/ subdirs: manifest.json + qcow2)
    └── arm-translators/        # Downloaded ARM translators (libndk/libhoudini)
```

Runtime sockets live under `$XDG_RUNTIME_DIR/andler/qmp/` (0700): the per-instance QMP socket (`<id>.sock`), its dedicated event monitor (`<id>.sock.events.sock`) and the guest-agent chardev socket (`<id>.qga.sock`).

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
docker compose -f docker/e2e/compose.yaml run --rm integration-test

# Or directly (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored
```

### E2E Suite

`docker/e2e/e2e.sh` orchestrates the suite: it starts a fresh `andlerd` on an isolated SQLite store and runs each `docker/e2e/tests/NN_*.sh` suite in order, printing an assertion count per suite and a final summary (non-zero exit on any failure). Each suite is self-contained and leaves no instances behind.

Coverage (all CLI commands):

1. Lifecycle + FSM negatives: create from TOML/flags, list/status (text + `--json`, short-id resolution), start/pause/resume/stop, double-start/double-stop, remove-while-running, metrics (`--once`, `--json`), log streaming (SIGTERM line captured, empty after stop), remove with/without `--purge`
2. Config: `config view`, `config set` (name, `display.resolution`, malformed value, unknown key), `config edit` via `$VISUAL` (success + broken TOML)
3. Disk: create/info/resize (grow + shrink refusal)/compact (qcow2 + raw), zero-size and unparsable-size negatives
4. Snapshots: live create (incl. duplicate tag)/list/`--json`, offline restore, restore-while-running, create/delete-while-stopped
5. Clone/export: Android create (incl. missing base image), linked/full-standalone/shared-base clones, live-clone removal protection, export, nonexistent-source negatives
6. Guest: error paths always; deep tests (offline install/remove/list against a real Debian rootfs through the libguestfs appliance, Android boot-mode switching) — SKIP when guestfish is unavailable, rest of the suite still runs
7. Client-side: `create --dry-run`, `--verify` pass/fail, wizard non-TTY refusal, shell completions, `andler doctor`
8. Persistence: daemon restart against the same store (state survives), final cleanup

```bash
docker compose -f docker/e2e/compose.yaml run --rm e2e
```

### gRPC Round-Trip Tests

`apps/daemon/src/grpc_roundtrip_test.rs` contains a suite of tests that verify the full gRPC pipeline:

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
│   │       ├── guest_tools.rs    # offline guest provisioning (libguestfs appliance)
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
├── apps/
│   ├── daemon/
│   │   └── src/
│   │       ├── main.rs           # Entry point, tonic server setup
│   │       ├── daemon/
│   │       │   ├── mod.rs        # Daemon struct, constructors, persist helpers (~250 lines)
│   │       │   ├── error.rs      # DaemonError enum (29 variants)
│   │       │   ├── types.rs      # InstanceRecord, SnapshotRecord, InstanceDirGuard
│   │       │   ├── instance_ops.rs  # create/start/stop/pause/resume/remove + create_linux_instance + resolve_instance_id
│   │       │   ├── clone_ops.rs  # clone_instance, export, find_live_clones
│   │       │   ├── snapshot_ops.rs  # create/restore/delete/list snapshots
│   │       │   ├── query_ops.rs  # status, list, get_config, stream, update_instance_config
│   │       │   ├── health_ops.rs # periodic VM health checks (ANDLERD_HEALTH_CHECK_INTERVAL_SECS)
│   │       │   └── tests/        # 12 test modules (incl. gRPC round-trip tests)
│   │       ├── service.rs        # DaemonService (gRPC wrapper)
│   │       └── grpc_roundtrip_test.rs  # integration tests, real TCP
│   │
│   └── cli/
│       └── src/
│           ├── main.rs           # CLI dispatch + clap enums
│           ├── instance_file.rs  # TOML config parser
│           ├── create.rs         # Create command
│           ├── status.rs         # Status, List, Config, Logs, Metrics
│           ├── snapshot.rs       # Snapshot commands
│           ├── disk.rs           # Disk commands
│           ├── lifecycle.rs      # Start, Stop, Pause, Resume, Remove
│           ├── clone.rs          # Clone, Export
│           ├── guest.rs          # Guest package management + boot mode
│           ├── doctor.rs         # Environment diagnostics
│           ├── edit.rs           # `config edit` — editor-based config editing
│           ├── preview.rs        # `create --dry-run` resolution
│           ├── verify.rs         # `create --verify` checks
│           ├── wizard/           # Interactive wizard
│           │   ├── mod.rs        # Wizard entry point, handle_wizard()
│           │   ├── basic.rs      # BasicResult, ask_kind, ask_name, ask_iso, ask_disk
│           │   ├── advanced.rs   # AdvancedConfig, 16 ask_* functions
│           │   └── summary.rs    # SummaryAction, print_summary
│           └── helpers.rs        # parse_size, format_size, format_bytes, ensure_qcow2_extension
│
├── docker/
│   ├── e2e/                      # Containerized test harness
│   │   ├── Dockerfile            # Multi-stage build + test targets
│   │   ├── compose.yaml          # unit-test / integration-test / e2e / daemon
│   │   ├── e2e.sh                # E2E suite orchestrator
│   │   └── tests/                # Per-feature suites (common.sh, NN_*.sh)
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
5. Add CLI flag in the appropriate module (`apps/cli/src/create.rs`, `apps/cli/src/lifecycle.rs`, etc.) if applicable
6. Add TOML field in `apps/cli/src/instance_file.rs` if applicable
7. Add test for the new field

### Adding a New Hypervisor Backend

1. Create `backends/andler-newbackend/` crate
2. Add to `Cargo.toml` workspace members
3. Implement `HypervisorBackend` trait from `andler-core`
4. Register in `Daemon::new()` / `default_backends()` in `apps/daemon/src/daemon/mod.rs`
5. Add `BackendKind` variant if needed in `andler-core`
6. Write tests (unit + integration)

### Adding a New gRPC Method

1. Add RPC definition in `services/andler-rpc/proto/andler.proto`
2. Add request/response message types if new
3. Add conversion functions in `services/andler-rpc/src/convert.rs`
4. Add daemon method in `apps/daemon/src/daemon/mod.rs` (or appropriate ops file)
5. Add gRPC handler in `apps/daemon/src/service.rs`
6. Add CLI command in the appropriate module (`apps/cli/src/create.rs`, `apps/cli/src/lifecycle.rs`, etc.)
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

## Releasing

Releases are driven by a `v*` tag: `.github/workflows/release.yml` builds the
binaries, packages them, and publishes the GitHub release.

1. **Green `main`.** The full gate must pass on the commit you are about to tag
   (`cargo build/test/clippy/fmt`) plus the E2E suite
   (`docker compose -f docker/e2e/compose.yaml run --rm e2e`).
2. **Promote the changelog.** In `docs/CHANGELOG.md`, turn the `[Unreleased]`
   section into `## [<version>] - <YYYY-MM-DD>` and leave a fresh, empty
   `[Unreleased]` above it. The release notes are extracted from that section,
   so write it for users, not for the diff.
3. **Bump the version.** `version` under `[workspace.package]` in the root
   `Cargo.toml` — every crate inherits it.
4. **Tag and push.**

   ```bash
   git tag -a v0.1.0 -m "ANDLER 0.1.0"
   git push origin v0.1.0
   ```

   The workflow publishes three archives — `andler-<tag>-linux-x86_64.tar.gz`
   (both binaries plus `LICENSE` and `README.md`), `andlerd-<tag>-linux-x86_64.tar.gz`
   (daemon only) and `andler-cli-<tag>-linux-x86_64.tar.gz` (CLI only) — each
   with a `.sha256` sidecar and covered by a build-provenance attestation. The
   release is titled with the tag itself and marked the latest release. The
   automated base-image builds use `base-image-*` tags with `--latest=false`,
   so they never shadow a project release.
5. **Verify the published artifacts.** Download the archive, check
   `sha256sum -c`, run `andler --version` and `andlerd --version` (the workflow
   runs the same smoke check before publishing), and confirm `releases/latest`
   resolves to the new tag.
6. **Afterwards.** Move the released items to *Shipped* in `docs/ROADMAP.md`
   and update the README if the install instructions changed.

Re-running the workflow for an existing tag re-uploads the assets with
`--clobber` rather than failing, so a publish that died partway is recoverable
without burning a new tag.

## Contributing

- Follow the Rust API guidelines
- Write tests for new functionality
- Keep backward compatibility in mind when changing APIs

## Getting Help

- Check existing issues and documentation
- Ask questions in the project repository

## License

GPL-3.0
