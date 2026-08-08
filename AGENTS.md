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
| `apps/daemon/` | Background service: orchestrates backends, FSM transitions, gRPC server |
| `apps/cli/` | Thin gRPC client: one subcommand = one gRPC request + print response |
| `docker/e2e/` | Containerized test harness: build env, compose targets, E2E suite |
| `docker/images/` | Guest base-image build pipelines (rootfs → bootable qcow2) |
| `docs/` | Architecture, development guide, API reference, gRPC reference, changelog, roadmap |
| `scripts/` | systemd service unit, installation script |

Every crate directory has its own `README.md` with crate-local behavior and integration notes (e.g. QEMU wire schemas in `backends/andler-qemu/README.md`). Read the relevant crate README before modifying that crate's code.

## Development Commands

### Feature Delivery Loop (mandatory order)

Every user-facing change (feature, fix, refactor with observable behavior) goes through this loop, in order, in one turn — the agent writes the code AND the tests AND the E2E AND the docs itself, verifies, then commits:

1. **Research** — read the crate READMEs and owning docs for every file you will touch; read all existing callers of shared infrastructure (see "Reuse existing patterns, including their hidden preconditions" in Refactoring Discipline) before writing anything.
2. **Code** — implement the change.
3. **Tests** — write/extend unit tests (realistic fixtures, see Test Patterns), gRPC round-trip tests for RPC changes, convert tests for proto changes.
4. **E2E** — add or extend `docker/e2e/tests/NN_*.sh` for user-visible features; never leave a feature without an E2E suite that exercises its positive path (a suite that only asserts error paths is not coverage).
5. **Docs** — update every owning doc in the same commit (see Documentation Sync); grep the docs for stale claims about the changed behavior.
6. **Gate** — run the four Pre-Merge Verification steps; then re-run after any subsequent edit.
7. **Commit** — per the Commit policy below; the commit message names the behavior change, the tests, and the docs.

Steps 4–7 are not optional follow-ups; a change is not done until the loop is complete.

### Build

```bash
# Local build
cargo build --workspace
cargo build --release

# Docker build (recommended for reproducibility; incremental thanks to BuildKit cache mounts)
docker compose -f docker/e2e/compose.yaml build unit-test
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
docker compose -f docker/e2e/compose.yaml run --rm unit-test

# Integration tests (requires qemu-img + /dev/kvm)
cargo test --workspace -- --ignored

# gRPC round-trip tests (real TCP, no QEMU required)
cargo test -p andler-daemon grpc_roundtrip

# E2E suite
docker compose -f docker/e2e/compose.yaml run --rm e2e
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

**Commit policy**: the agent commits autonomously once the full Feature Delivery Loop is complete — all four gate steps green, tests/E2E/docs for the change in place, working tree contains only this change. The user has asked for self-driving commits; do not stop to ask "may I commit?" after every change. Exceptions where the agent MUST ask first:
- destructive changes (deleting code/features/files, changing data formats, removing public API)
- a change that cannot pass the full gate in this environment (see the fallback rules above)
- anything the user explicitly asked to review before committing

Commit hygiene:
- **Conventional Commit messages**: `feat(scope): …`, `fix(scope): …`, `refactor(scope): …`, `docs(scope): …`, `test(scope): …`, `chore(scope): …` (scope = crate/area: `qemu`, `disk`, `daemon`, `cli`, `rpc`, `core`, `store`, `net`, `e2e`, `docs`). Subject ≤ 72 chars, imperative, English. Body explains the behavior change, why, and the test/docs coverage — not a restatement of the diff.
- **One logical change per commit.** If the loop produced two independent changes, make two commits (split the staging; docs travel with their change).
- Never commit work-in-progress, broken builds, or untested changes; never commit unrelated files (`git status` must show only this change's files).

**Done means**: all four gate steps pass, tests (unit + round-trip + E2E where applicable) are in place, the docs describing the changed behavior are updated in the same commit (see Documentation Sync), and the commit has been made.

**Branching**: day-to-day work (features, fixes, doc-only changes) commits straight to `main` — every commit already passes the full gate (see Commit policy above), so `main` stays green at every commit and there is no batching step that needs a branch. A **multi-phase architecture rework tracked by a temporary top-level plan file** — created for the refactor and deleted once it lands, see Temporary Plan Documents below — is the one case that uses branches, because a phase can span many commits over days/weeks and `main` must stay releasable throughout:

- One branch per phase, `refactor/<phaseN>-<slug>` (e.g. `refactor/phase0-foundations`), cut from `main` when the phase starts.
- **Every commit on a phase branch still passes the full four-step gate** — a phase branch is not a WIP dumping ground with a looser bar; it holds the same standard as `main`, just not yet merged. This is what makes the next two points possible.
- **Phase-exit gate, in addition to the per-commit gate**: before merging a phase branch into `main`, run the full E2E suite (not only unit + round-trip), re-check the phase's own guardrail items in the plan document, and update the plan's own phase-status marker in the merge commit.
- **Merge by fast-forward or rebase, never squash.** Squash-merging exists to hide messy WIP history; this repo's per-commit gate already guarantees there is no messy history to hide, so squashing would only destroy bisectability that was paid for one commit at a time. If `main` has moved during the phase, rebase the phase branch onto it (never merge `main` into the phase branch) and re-run the gate after the rebase — a rebase that changes no code still needs the gate re-run, because the new base can change behavior (dependency bumps, a sibling phase's changes).
- **Independent phases get independent branches off `main`**, not off each other — if the plan document marks two phases as not depending on each other, branch and merge them separately; only rebase one on the other if they end up touching the same files.
- Because every merge to `main` is a single, fully-gated, atomic phase, `git revert <merge-commit>` is always a safe way to back out a phase that turns out to have a problem after merging — this is the rollback path for any phase, not something to design separately per phase.

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
- A non-obvious `unsafe` block requires a safety justification, written as `// SAFETY: …` directly above the block — already the convention at the three existing call sites (`backends/andler-qemu/src/process.rs`, `backend.rs`, `metrics.rs`). Enforce it mechanically, not just by review: add `undocumented_unsafe_blocks = "warn"` under a `[workspace.lints.clippy]` table in the root `Cargo.toml`, which the existing `cargo clippy --workspace -- -D warnings` gate then turns into a hard failure. All three existing sites already comply, so turning this on costs nothing today — but new `unsafe` is arriving soon (pidfd-based process supervision), and it should not be the first to skip the convention.
- A complex algorithm needs a one-line "what this does" note
- A `#[allow(...)]` lint suppression needs a brief reason

**Exception: documenting a rejected approach, sized to the hazard, not capped at 2 lines.** When a previous implementation was actively harmful (not just wrong) and the failure mode is not obvious from reading the current code, the comment may run longer than 1–2 lines — long enough to say what was tried, why it was dangerous, and what real fix is still missing. The canonical example already in this repo is `services/andler-net/src/lib.rs`, `setup_isolated`: the previous version silently dropped all inbound host traffic while providing no actual VM isolation, and the current stub's comment explains that so nobody "fixes" the stub by resurrecting the dangerous version without reading history first. The test for this exception is not length, it's stakes: would a future editor, seeing only the current code, plausibly reintroduce a real bug? If yes, write what they need to not do that; if the current code is simply not-yet-implemented for an ordinary reason, a short note plus a `docs/ROADMAP.md`/tracking-issue reference is enough and belongs under the normal 1–2 line rule instead.

**All other detailed documentation belongs in `docs/`** — not in code comments. Every module, trait, struct, and public function is documented in the appropriate `docs/*.md` file or crate-level `README.md`. The implementer writes the code, then writes (or updates) the docs/ file to describe it.

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
6. `apps/cli/src/instance_file.rs` — TOML parsing (plus a `create.rs` flag or wizard step when user-facing)
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
- **A dependency that changes the binary's weight class needs a one-line justification in the commit, not just a `Cargo.toml` diff.** Most additions are ordinary (another `thiserror`-shaped crate costs nothing to reason about); the exception is a dependency that brings its own large static payload, a new system-level requirement, or a new privileged surface — the kind that changes what `doctor` has to check or what the install script has to ship. Concrete upcoming case: statically-linked `guestfs-tools` for offline guest mutation is exactly this class; `libc` for raw syscalls is not (already a thin, already-vetted dependency doing the same job it does today). The justification is one sentence — what it costs (binary size, build time, a new runtime requirement) and why the alternative was worse — not a defense, just a record so the next reader isn't left reverse-engineering the decision from a diff.
- **No silent fallbacks on invalid input.** A missing/unsupported value must produce an explicit error, never a quietly different behavior. Real bugs: `android_version = 12` in a TOML file silently created an Android 13 VM; `parse_size("0")` created a 0-byte disk; `CdromBus::Unspecified` used to fall back to IDE. When a fallback is intentional (e.g. `NatBackend::Unspecified → Slirp`), it must be a documented decision, not a `default()` that hides the input.

- **Never discard a `Result` whose operation has external effects.** `let _ = ...` / `.ok()` are only acceptable on pure code. Commands that touch the system (`qemu-nbd --disconnect`, `umount -l`, `apt-get update`, `dnf`, `pacman`) must have their errors surfaced or at least logged — real bugs: failed `qemu-nbd --disconnect` left stale NBD devices attached; a failed package-index update was reported as success. When a cleanup error is deliberately non-fatal, log it and say why.

- **No `.unwrap()`/`.expect()` on production paths** (parser input, paths, files). They panic on data the operator controls; return the error instead. Real bug: `instance_file.rs` panicked with `.expect()` instead of reporting a missing field. `unwrap()`/`expect()` are allowed in `#[cfg(test)]` code and in expressions whose invariants are proven two lines above and commented.

- **Cross-crate duplication must be hoisted, not copied.** If the same logic (parsing, formatting, path resolution) is needed in two crates, it belongs in `andler-core` or in one crate re-used via a workspace dependency — never a second copy. Real bug: the same validation was fixed twice in `cli` and `daemon` because each had its own copy.

- **Reuse existing patterns, including their hidden preconditions.** Before writing code against shared infrastructure (NBD mounts, QMP, persistence, guest filesystem), read every existing caller of that infrastructure — identical-looking operations can require different privileges or state. Real bug: `arm_translator.rs` mutated the rw-mounted guest partition with raw `std::fs` while `guest_tools.rs`/`boot_mode.rs` already routed mutations through `sudo -n`; the raw path failed with EPERM on the guest's root-owned directories and the feature had never worked.

- **All network and external-process I/O must have timeouts.** `reqwest::get` with no timeouts hung for minutes on a filtered network; a request without a total timeout can hang forever on a connection that accepts but never answers. Set connect + total timeouts on every HTTP call, and surface failures as actionable errors (what to do next, not just what failed).

- **Test fixtures must use real input shapes.** A unit test that feeds a shortened stand-in for real data can mask length/format bugs. Real bug: the bridge tap name test used an 8-char fake id, so `tap{64-hex-id}-eN` (70 chars, over Linux's 15-char IFNAMSIZ) passed CI and bridge mode never worked. When the production input has a fixed form (64-hex instance id, absolute paths, real size suffixes), at least one test must use that exact form.

- **Async tests must not deadlock.** Never hold a `tokio` lock guard across an `.await` that takes the same lock — a read guard held in the same task while another call awaits `write()` hangs forever, and the test harness reports a hang, not a failure. Scope guards in blocks (`{ let g = lock.read().await; ... }`) whenever a later call in the same test takes the lock. Run new async tests with a `timeout` once; a test that does not finish is a bug, not a slow test.

- **A feature's tests must fail on its real bug.** If the only way a test passes is by not exercising the changed path (mock returns early, assertion greps a substring that also matches unrelated output), rewrite it. Real bug: the E2E suite tested bridge mode only as an error path, so the IFNAMSIZ failure of the positive path went unnoticed — every user-visible feature needs an E2E positive path.

- **A module that keeps growing is a design signal, not a target.** Real example already in this repo: `backends/andler-qemu/src/cmdline.rs` (1000+ lines) and `services/andler-rpc/src/convert.rs` (1600+ lines) grew one small, individually-reasonable addition at a time until each was doing several unrelated jobs (cmdline.rs: arg-building for every device category in one function soup; convert.rs: every message's conversion in one file) — nobody made a bad call in any single commit, the size itself became the bug (hard to review a diff against 1000 lines of context, hard to find the one function that matters). When a change would push a module past roughly 600–800 lines, that commit's job is not just to add the feature — it's to also propose the seam to split along (the module usually already has one: cmdline.rs splits by device category, convert.rs by message group), even if the actual split lands in a later commit. Don't let "it's just one more function" be the reasoning for the tenth time in a row.

## Documentation Sync

Docs drift is a bug, same severity as a failing test. Rules that keep the repo honest:

**Pre-commit checklist (run in order, before `git commit`):**
1. `git status` — only this change's files are present (see Commit policy)
2. `grep` the docs for every changed observable (error text, flag name, RPC, config key, count) — stale claims are bugs
3. Confirm the owning doc of each touched area was updated: crate README for crate-local behavior, `docs/GRPC_API.md` for RPCs/status codes, `docs/API.md` for CLI/TOML, `docs/ARCHITECTURE.md` for mechanisms, `docs/CHANGELOG.md` (Unreleased) for user-visible changes, `docs/ROADMAP.md` when a planned item ships
4. Confirm the E2E suite list (`docker/e2e/README.md`) and this file's docs map stayed accurate

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
- **Removing a component** (crate, RPC, CLI command, config key) is a behavior change too: update the owning docs and grep the whole repo for stale references — crate graphs (AGENTS.md, `docs/ARCHITECTURE.md`), README trees and crate-doc lists, `docs/GRPC_API.md` value tables, `docker/e2e/Dockerfile` COPY lines and stub loops, e2e scripts.

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
| `docker/e2e/README.md` | docker build/test/e2e targets, containerized verification |
| `backends/andler-qemu/README.md` | QEMU integration facts and Known Limitations |
| other crate `README.md` | crate-local behavior |

A new file in `docs/` must be registered here in the same commit.

### Temporary plan documents

A multi-phase architecture rework (see Branching) is tracked by a plan file created at the top level for the duration of that refactor only. It is **never added to the docs map above and never cited by name** — not from code, not from commit messages, not from any file in `docs/`, not from this file — because the file is deleted once the refactor lands, and a citation naming it becomes a dangling reference the moment it's gone. This is not hypothetical: this repo's own git history already shows a previous architecture-refactor plan file being created and later deleted — the discipline below is what keeps the next one from leaving a dangling trail behind.

- Commit messages reference the phase by number and slug (`Phase 2 (resolver-access)`, matching the branch name — see Branching), never the plan file's name.
- A decision that must be checkable after the refactor completes is written into the doc that owns that kind of content (see Docs map above) as part of that phase's exit checklist — it does not live only in the plan file.
- The plan file's own phase-exit process defines when each landed section shrinks to a pointer, and — on the last phase — when the file is deleted outright and the repository is grepped for its filename to confirm no reference survived it.

## Important Files

### Entry Points

- `apps/daemon/src/main.rs` — daemon binary entry, tonic server setup, signal handling
- `apps/cli/src/main.rs` — CLI dispatch, clap enums, gRPC client setup

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
4. `apps/daemon/src/daemon/<ops>.rs` — the daemon method
5. `apps/daemon/src/service.rs` — `DaemonService` handler + `DaemonError` → `Status` mapping arm
6. `apps/cli/src/main.rs` enum + handler file (thin 1:1 request → print response)
7. `apps/daemon/src/grpc_roundtrip_test.rs` — round-trip test over real TCP
8. docs in the same commit: `docs/GRPC_API.md`, `docs/API.md`, `docs/CHANGELOG.md` (Unreleased)

### Persistence

- `services/andler-store/src/store.rs` — SQLite store (instances + snapshots), `Arc<Mutex<Connection>>`

### Daemon Logic

- `apps/daemon/src/daemon/mod.rs` — `Daemon` struct, constructors, backend registry
- `apps/daemon/src/daemon/instance_ops.rs` — create/start/stop/pause/resume/remove
- `apps/daemon/src/daemon/clone_ops.rs` — clone/export/find_live_clones
- `apps/daemon/src/daemon/snapshot_ops.rs` — disk-only snapshots: live create/delete over QMP `blockdev-snapshot-internal-sync`/`-delete-internal-sync`; offline restore via `qemu-img snapshot -a` (instance must be stopped; no live revert exists)
- `apps/daemon/src/daemon/health_ops.rs` — periodic VM health checks (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s, 0 disables)
- `apps/daemon/src/daemon/query_ops.rs` — status, list, metrics streaming
- `apps/daemon/src/daemon/types.rs` — InstanceRecord, SnapshotRecord, InstanceDirGuard
- `apps/daemon/src/daemon/error.rs` — `DaemonError` (30+ variants) → gRPC status mapping
- `apps/daemon/src/service.rs` — `DaemonService` (thin gRPC wrapper)
- `apps/daemon/src/grpc_roundtrip_test.rs` — 30 integration tests (real TCP; count drifts with each PR)

### CLI Commands

- `apps/cli/src/create.rs` — Create instance (TOML or CLI flags), `--dry-run`/`--verify`
- `apps/cli/src/clone.rs` — `Clone`/`Export` commands (`CloneInstanceRequest` with Linked/FullStandalone/SharedBase modes, `ExportInstanceDiskRequest`)
- `apps/cli/src/instance_file.rs` — TOML `InstanceFile` parsing for `create --file` (android_version, base_image_path, ovmf_vars_path, overlay/gapps/microg/libndk)
- `apps/cli/src/status.rs` — Status, List, Config, Logs, Metrics (`--json` on status/list/metrics)
- `apps/cli/src/disk.rs` — disk create/info/resize/compact (action flags mutually exclusive)
- `apps/cli/src/snapshot.rs` — snapshot create/restore/delete/list (`--json` before the subcommand)
- `apps/cli/src/guest.rs` — guest install/remove/list/boot-mode (online via QGA chardev `*.qga.sock`; offline qemu-nbd fallback)
- `apps/cli/src/doctor.rs` — environment checks (KVM/QEMU/OVMF/nbd/sudoers/daemon/base images)
- `apps/cli/src/lifecycle.rs` — start/stop/pause/resume/remove (remove `--purge` confirms on TTY)
- `apps/cli/src/edit.rs` — `config view`/`edit` (opens the real `instance.toml` in `$VISUAL`/`$EDITOR`) and `config set` (whitelisted keys; `display.resolution` applies live via QGA, other keys → `InvalidConfigKey` — see `daemon::instance_ops::set_instance_config`)
- `apps/cli/src/preview.rs` — `--dry-run` client-side config/QEMU-cmdline resolution
- `apps/cli/src/verify.rs` — `--verify` pre-flight checks (paths, OVMF, disk size, GPU/CPU/memory)
- `apps/cli/src/wizard/` — Interactive wizard with hardware auto-detection
- `apps/cli/src/helpers.rs` — `parse_size`, `format_size`, `format_bytes`, `format_timestamp`, `which`, `ensure_qcow2_extension`

### Config & Build

- `Cargo.toml` — Workspace definition, shared dependencies
- `rust-toolchain.toml` — Stable channel, rustfmt + clippy components
- `docker/e2e/compose.yaml` — Unit test, integration test, E2E targets
- `docker/e2e/Dockerfile` — Multi-stage build + test harness
- `docker/e2e/e2e.sh` — E2E suite orchestrator (runs tests/NN_*.sh)

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
| `~/.andler/cache/base-images/` | Android/Linux base images (flat root or `android<version>-<variant>/` subdirs; manifest.json + qcow2 pairs) |
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
| `rand` | Instance ID generation (64-hex, docker-style) |

## Testing & QA

### Test Frameworks

- **Unit tests**: Built-in `#[cfg(test)]` modules in each crate
- **Integration tests**: `#[ignore]` attribute for tests requiring QEMU/KVM
- **E2E tests**: Orchestrated shell suite (`docker/e2e/e2e.sh` + `docker/e2e/tests/NN_*.sh`) with real daemon + CLI

### Test Counts

Per-crate test counts are NOT pinned in this file or in `docs/` — they drift with every PR and have repeatedly gone stale. `cargo test --workspace` is the single authoritative source. Approximate counts may appear in crate READMEs and are intentional; never "fix" them to exact numbers.

Suites worth knowing about (no numbers):
- `andler-core`: pure domain tests, no I/O, no QEMU — must stay fully testable offline
- `andler-daemon`: unit tests across 11 test modules in `apps/daemon/src/daemon/tests/` (+ `mod.rs`) + gRPC round-trip tests (real TCP, real tonic, no QEMU)
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
docker compose -f docker/e2e/compose.yaml run --rm unit-test
docker compose -f docker/e2e/compose.yaml run --rm integration-test
docker compose -f docker/e2e/compose.yaml run --rm e2e
```

### Test Patterns

- Domain tests (`andler-core`): Pure logic, no I/O, no QEMU
- Backend tests (`andler-qemu`): Mocked data, parsing/validation, reference config comparison
- Store tests (`andler-store`): In-memory SQLite (`:memory:`)
- gRPC round-trip tests: Real TCP, real protobuf, real tonic server/client
- CLI tests: TOML parsing, helper functions, error formatting
- **Trait-conformance tests, once a trait has two real (non-fake) implementations**: a shared test suite runs the same assertions against every implementation, so behavioral drift between them is caught as its own bug class, distinct from a bug in one implementation. `HypervisorBackend` has only ever had one real implementation (`andler-qemu`) so this hasn't been needed yet; it becomes necessary the day a second real implementation of any core trait exists.

**Test quality bar** (violations are review blockers):
- Every new test must fail on a plausible bug in the code it covers (a test that cannot fail is decoration)
- Fixtures use real input shapes: real 64-hex instance ids, real paths, real size strings — not shortened stand-ins (see Refactoring Discipline)
- No test may hang: lock guards scoped, no unbounded waits on sockets/processes; when a test waits on I/O it uses a bounded timeout and asserts the elapsed time
- Tests that touch the network run against a local listener, never a live external host
- A test that pins a known-limitation message must be updated (not deleted) when the limitation is fixed

### E2E Suite

`docker/e2e/e2e.sh` orchestrates the suite: it starts a fresh `andlerd` on an
isolated sqlite store and runs every `docker/e2e/tests/NN_*.sh` suite in
order (each self-contained and with its own assertion counter), then prints a
summary and exits non-zero when any suite fails. Coverage spans all CLI
commands: lifecycle + FSM negatives (start/pause/resume/stop, remove-while-
running, double-stop), config view/set/edit, disk create/info/resize/compact,
live snapshots + offline restore, clone/export (all three modes + removal
protection), guest install/remove/list and boot-mode (deep tests over a real
rootfs via qemu-nbd, SKIP when the `nbd` module is unavailable), hotplug
attach/detach disk+net on a live VM, dry-run/
verify/wizard/completions/doctor, and daemon-restart persistence. Each suite
leaves no instances behind.

### Coverage Expectations

- All new features require unit tests
- Domain logic (`andler-core`) must be fully testable without QEMU
- gRPC changes require round-trip test additions
- Integration tests (`#[ignore]`) for QEMU-dependent paths
- Lifecycle/QMP/snapshot/guest-agent changes additionally require a live E2E against a test daemon (separate port, see "Working with a live daemon") — unit tests pin wire formats and parsing, not real QEMU behavior (precedent: vmstate snapshot blocking by non-migratable devices was only discoverable live)
- **Every user-visible feature gets a `docker/e2e/tests/NN_*.sh` suite (or an extension of an existing one) that exercises its positive path** — error-path-only suites miss exactly the failures unit tests cannot see. The suite must be runnable in the containerized harness (SKIP guard when a kernel feature like `nbd` is unavailable) and must clean up after itself (no instances left behind)
