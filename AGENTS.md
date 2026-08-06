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
- **Backend abstraction**: `HypervisorBackend` trait defines hypervisor-agnostic interface — sync methods: `name`, `supported_render_backends`, `metrics_stream`, `log_stream`; async: lifecycle (`spawn`/`pause`/`resume`/`stop`/`status`), snapshots (`snapshot`/`snapshot_restore`/`snapshot_delete`/`snapshot_list`), guest operations (`is_guest_agent_available`, `set_guest_display_resolution`, `guest_exec_install`, `guest_exec_remove`, `guest_check_binary_installed`). Method counts drift with features — count from `core/andler-core/src/backend.rs`, never from this file. New backend = implement this trait
- **Explicit FSM**: Instance lifecycle governed by strict state machine (7 states, 7 events). No implicit transitions
- **Persistence optional**: Daemon works with or without SQLite. In-memory is source of truth for current session
- **No global state**: Each crate has clear responsibilities

### Crate Dependency Graph

```
andler-core          (no workspace dependencies — bottom layer)
    ↑
    ├── andler-qemu     (depends on: core)
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

Every crate directory has its own `README.md` with crate-local behavior and integration notes (e.g. QEMU wire schemas in `backends/andler-qemu/README.md`). Read the relevant crate README before modifying that crate's code.

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

**Commit policy**: the agent asks "may I commit?" with the message and file list, the user approves, then the agent runs `git add`/`git commit`. Never commit without explicit user approval; an unrequested commit is a workflow violation even when the change itself is good.

**Done means**: all four gate steps pass, the docs describing the changed behavior are updated in the same commit (see Documentation Sync), and the commit has been made after explicit user approval.

## Code Conventions & Common Patterns

### Error Handling

- **`thiserror`** for all domain errors (`BackendError`, `FsmError`, `DaemonError`, `StoreError`, `DiskError`, `ConvertError`)
- `BackendError::NotImplemented` for unimplemented backend methods (never `todo!()`/`unimplemented!()`)
- `DaemonError` (30+ variants, drifts with each PR — count from `error.rs`) → maps to gRPC status codes
- `ConvertError` in `andler-rpc` for proto↔domain conversion failures
- **Actionable error text**: user-facing errors (CLI output, gRPC messages) must instruct the user what to do next, not just describe the failure — e.g. `InstanceMustBeStopped` says "stop it first", `InstanceAlreadyStopped` points at `andler status`/`andler start`. Surfacing raw QEMU/io stderr verbatim is a UX bug: sanitize, truncate (`status_message()`, 384 chars), and explain.

### Async Patterns

- **`tokio`** runtime with `rt-multi-thread`, `macros`, `process`, `fs`, `net`, `sync`, `time`, `io-util`, `signal`
- **`async_trait`** for the async methods of `HypervisorBackend` (the sync ones: `name`, `supported_render_backends`, `metrics_stream`, `log_stream`)
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

**Adding or changing a config key/section** — the full chain, in one commit:
1. `core/andler-core/src/config/<section>.rs` — the field's type
2. `core/andler-core/src/config/instance.rs` — field on `InstanceConfig` + its default
3. `core/andler-core/src/config/mod.rs` — re-export
4. `services/andler-rpc/proto/andler.proto` — field on the `InstanceConfig` message
5. `services/andler-rpc/src/convert.rs` — both conversion directions; required request fields → `ConvertError::MissingField`
6. `cli/src/instance_file.rs` — TOML parsing (plus a `create.rs` flag or wizard step when user-facing)
7. tests: core domain + conversion round-trip
8. docs in the same commit: `docs/API.md` (TOML examples), `docs/GRPC_API.md`, `docs/CHANGELOG.md` (Unreleased); grep existing TOML examples for stale shapes

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

## Documentation Sync

Docs drift is a bug, same severity as a failing test. Rules that keep the repo honest:

- **Code and docs change together, in the same commit.** Any change that alters observable behavior — RPC surface, error text, config keys, CLI flags, lifecycle semantics, snapshot behavior — must update the docs that describe it in the same commit:
  - proto messages / gRPC status codes → `docs/GRPC_API.md`
  - CLI surface and behavior → `docs/API.md`
  - architecture, FSM, snapshot mechanism → `docs/ARCHITECTURE.md`
  - crate-local behavior → that crate's `README.md` (QEMU wire schemas and integration notes live in `backends/andler-qemu/README.md`)
  - user-visible feature changes → `docs/CHANGELOG.md` (Unreleased)
  - top-level feature list → root `README.md`
- **AGENTS.md is an index, not a knowledge base.** Technical detail lives in the doc that owns it; AGENTS.md only points at the owner. Never copy content into AGENTS.md — duplicated knowledge drifts in one of the copies.
- **No unverified numbers in docs.** Any count (variants, methods, tests, RPCs) must be checkable against the code at review time; prefer "count from the file" over a literal number. Pinned counts rot — test counts are already unpinned, and the same rule applies to `DaemonError` variants and `HypervisorBackend` methods.
- **When fixing behavior, grep the docs that describe it** (error text, section headers, README feature lists) and fix them in the same commit — a doc claim that contradicts code is a bug report waiting to happen.
- **Removing a component** (crate, RPC, CLI command, config key) is a behavior change too: update the owning docs and grep the whole repo for stale references — crate graphs (AGENTS.md, `docs/ARCHITECTURE.md`), README trees and crate-doc lists, `docs/GRPC_API.md` value tables, `docker/dev/Dockerfile.dev` COPY lines and stub loops, e2e scripts.

### Docs map

Where the living truths live — read the owner before writing the claim anywhere else:

| Doc | Owns |
|-----|------|
| `README.md` | top-level feature list, quick start |
| `docs/ARCHITECTURE.md` | architecture, FSM, snapshot mechanism |
| `docs/API.md` | CLI reference, TOML config examples |
| `docs/GRPC_API.md` | proto messages, gRPC status-code table |
| `docs/DEVELOPMENT.md` | development workflow, crate layout |
| `docs/CHANGELOG.md` | user-visible changes (Unreleased section) |
| `docs/ROADMAP.md` | planned work |
| `docker/dev/README.md` | docker build/test/e2e targets, containerized verification |
| `backends/andler-qemu/README.md` | QEMU integration facts and Known Limitations |
| other crate `README.md` | crate-local behavior |

A new file in `docs/` must be registered here in the same commit.

## Important Files

### Entry Points

- `daemon/src/main.rs` — daemon binary entry, tonic server setup, signal handling
- `cli/src/main.rs` — CLI dispatch, clap enums, gRPC client setup

### Core Domain

- `core/andler-core/src/backend.rs` — `HypervisorBackend` trait, `ResourceMetrics`, `BackendHandle`
- `core/andler-core/src/base_image.rs` — Android base-image auto-discovery (`base_image::resolve()`), used by the daemon when the client omits `base_image_path`
- `core/andler-core/src/android_profile.rs` — Android version/root/store profiles
- `core/andler-core/src/clone.rs` — CloneMode and clone-type domain logic
- `core/andler-core/src/fsm.rs` — `InstanceState` (7 states), `InstanceEvent` (7 events)
- `core/andler-core/src/config/` — per-section config modules: 9-section `InstanceConfig` (cpu, memory, disk, display, gpu, network, audio, input, firmware) in `instance.rs`; `cdrom.rs` defines `CdromBus`
- `core/andler-core/src/error.rs` — `BackendError`, `FsmError`
- `core/andler-core/src/paths.rs` — Unified path resolution (`runtime_dir()`, `db_path()`)

### Backend Implementation

- `backends/andler-qemu/src/cmdline.rs` — Pure function: `InstanceConfig` → QEMU CLI args
- `backends/andler-qemu/src/qmp.rs` — QMP protocol client (unix socket)
- `backends/andler-qemu/src/process.rs` — `QemuProcess` (spawn, logs, metrics)
- `backends/andler-qemu/src/metrics.rs` — `/proc`-based per-VM metrics poller
- `backends/andler-qemu/src/backend.rs` — QemuBackend: registry, spawn, QMP recovery (`diagnose_and_reset_qmp`)
- `backends/andler-qemu/README.md` — crate-local behavior and the owning doc for QEMU integration facts (Known Limitations: non-migratable vmstate, chardev socket rules, QGA vs QMP). Read it before touching QMP/QGA/snapshot/process code.

### RPC & Protocol

- `services/andler-rpc/proto/andler.proto` — all RPCs and message/enum definitions (count from the file; the CLI is a thin 1:1 wrapper)
- `services/andler-rpc/src/convert.rs` — Bidirectional proto↔domain conversions (1000+ lines)

**Adding or changing an RPC** — the full chain, in one commit:
1. `services/andler-rpc/proto/andler.proto` — message/enum + `rpc` stub
2. build (prost via the crate's build.rs, requires `protoc`)
3. `services/andler-rpc/src/convert.rs` — explicit `From`/`TryFrom`; request `UNSPECIFIED` → `ConvertError::MissingField` (never a silent default)
4. `daemon/src/daemon/<ops>.rs` — the daemon method
5. `daemon/src/service.rs` — `DaemonService` handler + `DaemonError` → `Status` mapping arm
6. `cli/src/main.rs` enum + handler file (thin 1:1 request → print response)
7. `daemon/src/grpc_roundtrip_test.rs` — round-trip test over real TCP
8. docs in the same commit: `docs/GRPC_API.md`, `docs/API.md`, `docs/CHANGELOG.md` (Unreleased)

### Persistence

- `services/andler-store/src/store.rs` — SQLite store (instances + snapshots), `Arc<Mutex<Connection>>`

### Daemon Logic

- `daemon/src/daemon/mod.rs` — `Daemon` struct, constructors, backend registry
- `daemon/src/daemon/instance_ops.rs` — create/start/stop/pause/resume/remove
- `daemon/src/daemon/clone_ops.rs` — clone/export/find_live_clones
- `daemon/src/daemon/snapshot_ops.rs` — disk-only snapshots: live create/delete over QMP `blockdev-snapshot-internal-sync`/`-delete-internal-sync`; offline restore via `qemu-img snapshot -a` (instance must be stopped; no live revert exists)
- `daemon/src/daemon/health_ops.rs` — periodic VM health checks (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s, 0 disables)
- `daemon/src/daemon/query_ops.rs` — status, list, metrics streaming
- `daemon/src/daemon/types.rs` — InstanceRecord, SnapshotRecord, InstanceDirGuard
- `daemon/src/daemon/error.rs` — `DaemonError` (30+ variants) → gRPC status mapping
- `daemon/src/service.rs` — `DaemonService` (thin gRPC wrapper)
- `daemon/src/grpc_roundtrip_test.rs` — 30 integration tests (real TCP; count drifts with each PR)

### CLI Commands

- `cli/src/create.rs` — Create instance (TOML or CLI flags), `--dry-run`/`--verify`
- `cli/src/clone.rs` — `Clone`/`Export` commands (`CloneInstanceRequest` with Linked/FullStandalone/SharedBase modes, `ExportInstanceDiskRequest`)
- `cli/src/instance_file.rs` — TOML `InstanceFile` parsing for `create --file` (android_version, base_image_path, ovmf_vars_path, overlay/gapps/microg/libndk)
- `cli/src/status.rs` — Status, List, Config, Logs, Metrics (`--json` on status/list/metrics)
- `cli/src/disk.rs` — disk create/info/resize/compact (action flags mutually exclusive)
- `cli/src/snapshot.rs` — snapshot create/restore/delete/list (`--json` before the subcommand)
- `cli/src/guest.rs` — guest install/remove/list/boot-mode (online via QGA chardev `*.qga.sock`; offline qemu-nbd fallback)
- `cli/src/doctor.rs` — environment checks (KVM/QEMU/OVMF/nbd/sudoers/daemon/base images)
- `cli/src/lifecycle.rs` — start/stop/pause/resume/remove (remove `--purge` confirms on TTY)
- `cli/src/edit.rs` — `config view`/`edit` (opens the real `instance.toml` in `$VISUAL`/`$EDITOR`) and `config set` (whitelisted keys; `display.resolution` applies live via QGA, other keys → `InvalidConfigKey` — see `daemon::instance_ops::set_instance_config`)
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

### Environment Variables

| Variable | Applies to | Purpose |
|----------|-----------|---------|
| `ANDLERD_LISTEN_ADDR` | daemon | Listen address (default `127.0.0.1:50051`) |
| `ANDLERD_STORE_PATH` | daemon | SQLite database path (default `~/.andler/andlerd.db`) |
| `ANDLERD_LOG_FORMAT=json` | daemon | Structured JSON logging (default human-readable) |
| `ANDLERD_OVMF_CODE` | daemon | Override OVMF code path |
| `ANDLERD_OVMF_VARS` | daemon | Override OVMF_VARS template path |
| `ANDLERD_HEALTH_CHECK_INTERVAL_SECS` | daemon | Health-check interval (default 30s, `0` disables) |
| `RUST_LOG` | daemon | tracing filter (overrides `-v`/`-vv` verbosity) |
| `ANDLERD_ADDR` | cli | Daemon address (default `http://127.0.0.1:50051`); `--daemon-addr` flag overrides |
| `ANDLER_HOME` | both | Root of all andler data (default `~/.andler`) |
| `ANDLER_WIZARD_NOT_TTY` | cli | Test override: force non-TTY wizard behavior |

### Runtime Paths

| Path | Purpose |
|------|---------|
| `~/.andler/instances/<id>/` | Instance home: `instance.toml`, `disk.qcow2`, `VARS.fd`, `console.log`, `qemu.log` |
| `~/.andler/cache/base-images/` | Android/Linux base images |
| `~/.andler/andlerd.db` | Default SQLite store |
| `$XDG_RUNTIME_DIR/andler/qmp/<id>.sock` | Per-instance QMP control socket |
| `$XDG_RUNTIME_DIR/andler/qmp/<id>.qga.sock` | Per-instance guest-agent (QGA) chardev socket |

### Working with a live daemon

- **Test daemons run on a separate port** — e.g. `ANDLERD_LISTEN_ADDR=127.0.0.1:50052` with its own `ANDLERD_STORE_PATH`. Never share a store or interfere with the auto-started daemon on 50051; after a test, remove the test instance with `remove --purge` and stop the test daemon.
- **`--daemon-addr` requires a scheme** — `http://127.0.0.1:50052`, not a bare `127.0.0.1:50052` (a bare address fails with "andlerd is not running").
- **`ANDLERD_STORE_PATH` must live in a user-owned directory** — `ensure_private_dir` chmods the parent 0700 and fails with `PermissionDenied` on root-owned directories like `/tmp`.
- **Never attach a second client to a live `qmp.sock`/`qga.sock`** — a QEMU chardev serves only the latest client, so a probe starves the daemon and hangs its operation (see Known Limitations in `backends/andler-qemu/README.md`).
- **`config edit` spawns `$VISUAL`/`$EDITOR`** — the variable's value is executed as a command (e.g. `VISUAL="sed -i s/OldValue/NewValue/ /path/to/instance.toml"`); in scripted/agent environments the default (`true`) would fail, so always set it explicitly.
- **A daemon restart loses live instances** — instances that were `Running` restore in `Error` state (backend handle gone); `start` is the documented recovery path and works from `Error`.
- **Diagnosing a failed start**: read the tail of `~/.andler/instances/<id>/qemu.log` and `console.log` (QEMU stderr/stdout/serial); `andler logs <id>` streams the same.
- **Instance stuck in `Error`**: the state message (from `andler status`) records why (health check, backend loss); after a daemon restart the `Error` record points at the original failure, and `start` retries.
- **daemon tracing**: run andlerd with `RUST_LOG=debug` (or `ANDLERD_LOG_FORMAT=json`) for request-level diagnostics; the CLI `-v`/`-vv` flags affect the client only.

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
- `andler-daemon`: unit tests across 11 test modules in `daemon/src/daemon/tests/` (+ `mod.rs`) + gRPC round-trip tests (real TCP, real tonic, no QEMU)
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
- Lifecycle/QMP/snapshot/guest-agent changes additionally require a live E2E against a test daemon (separate port, see "Working with a live daemon") — unit tests pin wire formats and parsing, not real QEMU behavior (precedent: vmstate snapshot blocking by non-migratable devices was only discoverable live)
