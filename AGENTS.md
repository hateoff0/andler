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
- **Backend abstraction**: `HypervisorBackend` trait defines hypervisor-agnostic interface (17 methods: 13 async + 4 sync — `name`, `supported_render_backends`, `metrics_stream`, `log_stream`). New backend = implement this trait
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
| `services/andler-disk/` | `qemu-img`/`qemu-nbd` wrappers: disk create/clone/resize/compact, offline guest provisioning (guest tools, ARM translators), free-space pre-check |
| `services/andler-net/` | Bridge/isolated network modes via `iproute2` |
| `services/andler-firmware/` | Hardware auto-detection, GPU metrics (NVIDIA/AMD/Intel), firmware discovery |
| `services/andler-store/` | SQLite persistence (two tables: `instances`, `snapshots`) |
| `services/andler-rpc/` | Protobuf definitions, gRPC generated code, proto↔domain conversions |
| `daemon/` | Background service: orchestrates backends, FSM transitions, gRPC server |
| `cli/` | Thin gRPC client: one subcommand = one gRPC request + print response |
| `docker/dev/` | Build environment, compose targets, E2E smoke test |
| `docker/images/` | Guest base-image build pipelines (rootfs → bootable qcow2) |
| `docs/` | Architecture, development guide, API reference, gRPC reference, changelog, roadmap |
| `scripts/` | systemd service unit, installation script |

## Development Commands

### Build

```bash
# Local build
cargo build --workspace
cargo build --release

# Docker build (recommended for reproducibility)
docker compose -f docker/dev/docker-compose.yml build --no-cache unit-test
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
docker compose -f docker/dev/docker-compose.yml run --rm unit-test

# Integration tests (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored

# gRPC round-trip tests (real TCP, no QEMU required)
cargo test -p andler-daemon grpc_roundtrip

# E2E smoke test
docker compose -f docker/dev/docker-compose.yml run --rm e2e
```

### Lint & Format

```bash
# Rust toolchain: stable (see rust-toolchain.toml)
# Components: rustfmt, clippy

cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
```

### Pre-Merge Verification (Mandatory)

**The full gate must pass before any change is considered done:**
1. `cargo build --workspace`
2. `cargo test --workspace`
3. `cargo clippy --workspace -- -D warnings`
4. `cargo fmt --all -- --check`

None of the four is optional and none is implied by "the code looks right." Broken, non-compiling code has previously been committed to this repo and stayed broken across multiple commits because nobody ran a build — treat that as the standard this rule exists to prevent. `#[allow(...)]` on clippy lints is acceptable with a one-line justification comment (see Code Comments Policy); match ergonomics use implicit `ref`, never explicit.

If the working environment cannot run a full build (e.g. no network access to crates.io, a `Cargo.lock` version newer than the available `cargo`, missing `protoc`), that is not a reason to skip verification silently:
1. Say explicitly that a build could not be performed and why.
2. Fall back to a manual, line-by-line trace of every call site touched: function signatures, struct/enum literal shapes (checked against their actual `struct`/`enum` definition, never assumed from a variable name), imports, and every caller of anything whose signature changed.
3. Ask for (or clearly flag the need for) a real `cargo build --workspace && cargo test --workspace` run before the change is trusted.

Never assume a struct or enum's shape (field names, tuple vs. struct variant, etc.) from how you'd expect it to look — open the actual definition and check.

## Code Conventions & Common Patterns

### Error Handling

- **`thiserror`** for all domain errors (`BackendError`, `FsmError`, `DaemonError`, `StoreError`, `DiskError`, `ConvertError`)
- `BackendError::NotImplemented` for unimplemented backend methods (never `todo!()`/`unimplemented!()`)
- `DaemonError` has 29 variants → maps to gRPC status codes
- `ConvertError` in `andler-rpc` for proto↔domain conversion failures

### Async Patterns

- **`tokio`** runtime with `rt-multi-thread`, `macros`, `process`, `fs`, `net`, `sync`, `time`, `io-util`, `signal`
- **`async_trait`** for the 13 async methods of `HypervisorBackend` (the other 4 trait methods are sync: `name`, `supported_render_backends`, `metrics_stream`, `log_stream`)
- `rusqlite` calls wrapped in `tokio::task::spawn_blocking` (blocking API)
- `tokio::sync::RwLock` for `Daemon.instances` (concurrent reads, rare writes)
- `tokio::sync::broadcast` for metrics streaming

### Serialization

- **`serde`** with `derive` for domain types (TOML, JSON)
- **`prost`/`tonic`** for gRPC protobuf (no serde on proto types)
- Proto↔domain conversions: explicit `From`/`TryFrom` impls, never auto-derived
- `UNSPECIFIED` proto values are rejected with `ConvertError::MissingField` on request conversion paths (`CreateInstanceRequest.cdrom_bus`, `android_version`, `clone_mode`, `cpu_priority`, `disk_format`, `display_engine`). Low-level `From<proto::X>` impls may map `Unspecified` to a domain default (`NatBackend::Unspecified → Slirp`) — never rely on that in request paths, the `TryFrom` request conversions check first

### Naming & Formatting

- **Rust 2021 edition**, stable toolchain

### Code Comments Policy

**Language: English only.** All code, comments, identifiers, error messages, commit messages, and documentation MUST be in English. No exceptions for convenience or "because the original author spoke X." Non-English text in code is a bug.
**Default: No comments in code.** The codebase is currently free of doc comments (`///`, `//!`) and inline comments. Keep it that way.

**Exception: 1–2 line comments** allowed ONLY when:
- A non-obvious `unsafe` block requires a safety justification
- A complex algorithm needs a one-line "what this does" note
- A `#[allow(...)]` lint suppression needs a brief reason

**All detailed documentation belongs in `docs/`** — not in code comments. Every module, trait, struct, and public function is documented in the appropriate `docs/*.md` file or crate-level `README.md`. The implementer writes the code, then writes (or updates) the docs/ file to describe it.

**Why:** Long doc comments in code rot faster than external docs (they're harder to find, harder to search, and split attention between two locations). Centralized docs/ files are easier to maintain, review, and keep accurate.
- Struct fields: `snake_case`, types: `PascalCase`
- Proto messages mirror domain types 1:1 by field
- CLI enums use `clap::ValueEnum` with `#[derive(Clone, Copy, PartialEq, Eq)]`

### Configuration Patterns

- **9-section `InstanceConfig`**: cpu, memory, disk, display, gpu, network, audio, input, firmware
- TOML instance files (auto-detect `LinuxVm` vs `AndroidVm` by content)
- Environment variables: `ANDLERD_*` prefix (`ANDLERD_LISTEN_ADDR`, `ANDLERD_STORE_PATH`, `ANDLERD_LOG_FORMAT`)
- CLI flags: `--daemon-addr`, `--kind`, `--file`, `--quick`, `--dry-run`, `--verify`, `--json`, `--full-id`

### Dependency Injection

- `Daemon::new()` / `with_store()` / `restore()` — three construction paths
- Backend registry: `HashMap<BackendKind, Arc<dyn HypervisorBackend>>`
- `default_backends()` function ensures consistent registry across constructors
- `Option<Store>` — persistence is opt-in, not mandatory

## Refactoring Discipline

Rules that exist because violating them has produced real bugs in this repo:

- **"No behavioral change" is a claim to verify, not assert.** Before calling a refactor purely structural, check whether any existing test's *assertion* (not just its call syntax) would need to change. If a test currently asserts one error variant and the refactor would make it assert a different one, that is a behavior change — say so explicitly, don't fold it into the diff quietly.
- **A test that pins a bug is still a bug.** If a test asserts on something documented as a known limitation (e.g. "flat string search matches nested field first"), fixing the underlying issue is expected to break that specific test. Rewrite the test to assert the *correct* behavior — do not preserve the old assertion just to keep the diff green, and do not silently leave the bug in place to avoid touching the test.
- **Shared helpers must have identical preconditions across all call sites, not just identical code shape.** Two functions can look byte-for-byte identical while relying on different state guarantees from their callers (e.g. one caller reachable from a "not yet started" state, another only reachable once running). Before merging duplicated blocks into one helper, check what state/error each call site is exercised under in its tests — if they differ, either don't merge, or split into a smaller shared helper plus a stricter wrapper.
- **`macro_rules!` fragment specifiers are not interchangeable.** `item` matches top-level items (fn, struct, impl, ...) — it does **not** match a bare struct field like `#[arg(long)] pub foo: bool,`. Use `tt` (token tree) repetition to splice arbitrary field-like syntax into a struct body.
- **Never guess a struct/enum's exact shape.** Check the real definition before writing a literal that constructs it (tuple variant vs. struct variant, exact field names). Getting this wrong is a compile error, not a style nit, and it's easy to miss when skimming.
- **Adding a new dependency**: check how other crates in the workspace already declare that dependency before deciding between `[workspace.dependencies]` and a direct per-crate version — match existing convention rather than introducing a second pattern for the same crate.
- **No silent fallbacks on invalid input.** A missing/unsupported value must produce an explicit error, never a quietly different behavior. Real bugs: `android_version = 12` in a TOML file silently created an Android 13 VM; `parse_size("0")` created a 0-byte disk; `CdromBus::Unspecified` used to fall back to IDE. When a fallback is intentional (e.g. `NatBackend::Unspecified → Slirp`), it must be a documented decision, not a `default()` that hides the input.

- **Never discard a `Result` whose operation has external effects.** `let _ = ...` / `.ok()` are only acceptable on pure code. Commands that touch the system (`qemu-nbd --disconnect`, `umount -l`, `apt-get update`, `dnf`, `pacman`) must have their errors surfaced or at least logged — real bugs: failed `qemu-nbd --disconnect` left stale NBD devices attached; a failed package-index update was reported as success. When a cleanup error is deliberately non-fatal, log it and say why.

- **No `.unwrap()`/`.expect()` on production paths** (parser input, paths, files). They panic on data the operator controls; return the error instead. Real bug: `instance_file.rs` panicked with `.expect()` instead of reporting a missing field. `unwrap()`/`expect()` are allowed in `#[cfg(test)]` code and in expressions whose invariants are proven two lines above and commented.

- **Cross-crate duplication must be hoisted, not copied.** If the same logic (parsing, formatting, path resolution) is needed in two crates, it belongs in `andler-core` or in one crate re-used via a workspace dependency — never a second copy. Real bug: the same validation was fixed twice in `cli` and `daemon` because each had its own copy.

## Important Files

### Entry Points

- `daemon/src/main.rs` — daemon binary entry, tonic server setup, signal handling
- `cli/src/main.rs` — CLI dispatch, clap enums, gRPC client setup

### Core Domain

- `core/andler-core/src/backend.rs` — `HypervisorBackend` trait (17 methods), `ResourceMetrics`, `BackendHandle`
- `core/andler-core/src/base_image.rs` — Android base-image auto-discovery (`base_image::resolve()`), used by the daemon when the client omits `base_image_path`
- `core/andler-core/src/android_profile.rs` — Android version/root/store profiles
- `core/andler-core/src/clone.rs` — CloneMode and clone-type domain logic
- `core/andler-core/src/fsm.rs` — `InstanceState` (7 states), `InstanceEvent` (7 events)
- `core/andler-core/src/config/` — 9 config modules (instance, cpu, memory, disk, display, gpu, network, audio, input)
- `core/andler-core/src/error.rs` — `BackendError`, `FsmError`
- `core/andler-core/src/paths.rs` — Unified path resolution (`runtime_dir()`, `db_path()`)

### Backend Implementation

- `backends/andler-qemu/src/cmdline.rs` — Pure function: `InstanceConfig` → QEMU CLI args
- `backends/andler-qemu/src/qmp.rs` — QMP protocol client (unix socket)
- `backends/andler-qemu/src/process.rs` — `QemuProcess` (spawn, logs, metrics)
- `backends/andler-qemu/src/metrics.rs` — `/proc`-based per-VM metrics poller
- `backends/andler-qemu/src/backend.rs` — QemuBackend: registry, spawn, QMP recovery (`diagnose_and_reset_qmp`)

### RPC & Protocol

- `services/andler-rpc/proto/andler.proto` — 26 RPCs, all message/enum definitions
- `services/andler-rpc/src/convert.rs` — Bidirectional proto↔domain conversions (1000+ lines)

### Persistence

- `services/andler-store/src/store.rs` — SQLite store (instances + snapshots), `Arc<Mutex<Connection>>`

### Daemon Logic

- `daemon/src/daemon/mod.rs` — `Daemon` struct, constructors, backend registry
- `daemon/src/daemon/instance_ops.rs` — create/start/stop/pause/resume/remove
- `daemon/src/daemon/clone_ops.rs` — clone/export/find_live_clones
- `daemon/src/daemon/snapshot_ops.rs` — snapshot CRUD via QEMU job API
- `daemon/src/daemon/health_ops.rs` — periodic VM health checks (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s, 0 disables)
- `daemon/src/daemon/query_ops.rs` — status, list, metrics streaming
- `daemon/src/daemon/types.rs` — InstanceRecord, SnapshotRecord, InstanceDirGuard
- `daemon/src/daemon/error.rs` — `DaemonError` (29 variants) → gRPC status mapping
- `daemon/src/service.rs` — `DaemonService` (thin gRPC wrapper)
- `daemon/src/grpc_roundtrip_test.rs` — 28 integration tests (real TCP)

### CLI Commands

- `cli/src/create.rs` — Create instance (TOML or CLI flags), `--dry-run`/`--verify`
- `cli/src/status.rs` — Status, List, Config, Logs, Metrics (`--json` on status/list/metrics)
- `cli/src/disk.rs` — disk create/info/resize/compact (action flags mutually exclusive)
- `cli/src/snapshot.rs` — snapshot create/restore/delete/list (`--json` before the subcommand)
- `cli/src/guest.rs` — guest install/remove/list/boot-mode (online QMP ↔ offline qemu-nbd fallback)
- `cli/src/doctor.rs` — environment checks (KVM/QEMU/OVMF/nbd/sudoers/daemon/base images)
- `cli/src/lifecycle.rs` — start/stop/pause/resume/remove (remove `--purge` confirms on TTY)
- `cli/src/edit.rs` — `config edit`: opens the real `instance.toml` in `$VISUAL`/`$EDITOR`
- `cli/src/preview.rs` — `--dry-run` client-side config/QEMU-cmdline resolution
- `cli/src/verify.rs` — `--verify` pre-flight checks (paths, OVMF, disk size, GPU/CPU/memory)
- `cli/src/wizard/` — Interactive wizard with hardware auto-detection
- `cli/src/helpers.rs` — `parse_size`, `format_size`, `format_bytes`, `format_timestamp`, `which`, `ensure_qcow2_extension`

### Config & Build

- `Cargo.toml` — Workspace definition, shared dependencies
- `rust-toolchain.toml` — Stable channel, rustfmt + clippy components
- `docker/dev/docker-compose.yml` — Unit test, integration test, E2E targets
- `docker/dev/Dockerfile.dev` — Multi-stage build environment
- `docker/dev/e2e_smoke.sh` — Full lifecycle E2E test

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
- **E2E tests**: Shell script (`docker/dev/e2e_smoke.sh`) with real daemon + CLI

### Test Counts

Per-crate test counts are NOT pinned in this file or in `docs/` — they drift with every PR and have repeatedly gone stale. `cargo test --workspace` is the single authoritative source. Approximate counts may appear in crate READMEs and are intentional; never "fix" them to exact numbers.

Suites worth knowing about (no numbers):
- `andler-core`: pure domain tests, no I/O, no QEMU — must stay fully testable offline
- `andler-daemon`: unit tests across 12 modules in `daemon/src/daemon/tests/` + gRPC round-trip tests (real TCP, real tonic, no QEMU)
- `andler-cli`: TOML parsing, helpers, wizard, create/status/disk/snapshot/guest commands
- `andler-qemu`: cmdline reference-config comparison, QMP wire-schema tests, `/proc` metrics parsing
- `andler-disk`: qemu-img parsing, NBD/mount helpers, translator staging (integration tests `#[ignore]`)

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
docker compose -f docker/dev/docker-compose.yml run --rm unit-test
docker compose -f docker/dev/docker-compose.yml run --rm unit-test -- --ignored
docker compose -f docker/dev/docker-compose.yml run --rm e2e
```

### Test Patterns

- Domain tests (`andler-core`): Pure logic, no I/O, no QEMU
- Backend tests (`andler-qemu`): Mocked data, parsing/validation, reference config comparison
- Store tests (`andler-store`): In-memory SQLite (`:memory:`)
- gRPC round-trip tests: Real TCP, real protobuf, real tonic server/client
- CLI tests: TOML parsing, helper functions, error formatting

### E2E Smoke Test

`docker/dev/e2e_smoke.sh` runs full lifecycle:
1. Start `andlerd` background process
2. Create Android instance (extracts the UUID from the `Created instance <name> (<id>)` output)
3. Negative checks: `status` of a nonexistent instance fails with "not found"; `disk create --size 0` is rejected; `create --disk-size-gib 0` is rejected by clap's range
4. Start instance
5. Stream metrics for 5 seconds
6. Stop instance (SIGTERM, asserts the qemu stderr line is visible in `andler logs`)
7. Remove instance with `--purge`
8. Verify clean shutdown

### Coverage Expectations

- All new features require unit tests
- Domain logic (`andler-core`) must be fully testable without QEMU
- gRPC changes require round-trip test additions
- Integration tests (`#[ignore]`) for QEMU-dependent paths
