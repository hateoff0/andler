# Roadmap

## Done

- [x] Domain model and backend abstraction (`andler-core`)
- [x] QEMU backend with full lifecycle management (`andler-qemu`)
- [x] QEMU VM snapshots — create/restore/delete/list via job API
- [x] Real-time resource metrics from `/proc` (CPU, RAM, disk, network)
- [x] AMD GPU metrics from sysfs (VRAM, GPU load)
- [x] NVIDIA GPU metrics via NVML + `nvidia-smi` fallback
- [x] Intel GPU metrics via sysfs `i915` (GPU load delta)
- [x] Offline Magisk provisioning via `qemu-nbd`
- [x] SQLite state persistence with cascade delete (`andler-store`)
- [x] gRPC protocol with 20 RPCs and bidirectional conversions
- [x] Factory reset with file cleanup (`remove --purge`)
- [x] Clone/export for Linux and Android VMs (3 modes)
- [x] Configurable per-instance snapshot timeout
- [x] Snapshot timeout per-operation override (`--timeout` flag)
- [x] Unified `create` command with `--kind linux`/`--kind android` + Android TOML
- [x] Monorepo restructure (core/backends/services/daemon/cli)
- [x] Comprehensive documentation in English
- [x] Disk management CLI (create, info, resize with shrink protection, compact)
- [x] Default disk size changed to 256 GiB
- [x] `--quick` mode — skip wizard, create with all defaults
- [x] `--cdrom-bus` flag (auto/virtio/ide)
- [x] `--compact-on-shutdown` feature (background task)
- [x] `--arm-translator` enum (none/libndk/libhoudini) replacing `--libndk`
- [x] Interactive wizard with smart defaults and hardware auto-detection
- [x] `andler edit` command — edit instance config via gRPC
- [x] Shell completions (bash, zsh, fish)
- [x] Partial instance IDs (Docker-style 8-char hex prefix)
- [x] Unified paths module (`andler-core::paths`)
- [x] `ensure_private_dir()` with 0700 permissions
- [x] `instance.toml` persistence alongside instances
- [x] `purge_instance_files()` with `remove_dir_all`
- [x] `resolve_instance_id()` — Docker-style partial ID resolution
- [x] `create_linux_instance()` — high-level resource creation
- [x] `update_instance_config()` — edit config via gRPC
- [x] `find_live_clones()` — clone source protection
- [x] Signal handling (SIGINT + SIGTERM)
- [x] systemd user unit + installation script
- [x] Colored status output with `IsTerminal` gating
- [x] NVML integration for NVIDIA GPU metrics
- [x] Snapshot limit (20 per instance)
- [x] `MalformedInstanceRef` error for invalid IDs
- [x] `CompactNotApplicable` error for raw format disks
- [x] `ShrinkRequiresConfirmation` error for disk shrink without `--shrink`
- [x] Log history from `qemu.log` before streaming live tail
- [x] Guest package management — `andler guest install/remove/list` with auto-fallback (online via QMP guest-exec, offline via qemu-nbd)

## In Progress

- (none currently)

## Roadmap

> **Priority decision**: the second backend (`andler-vmm`/Cloud Hypervisor)
> is explicitly the *last* thing on this roadmap, not medium-term. Until
> the existing QEMU backend and its UX are as close to ideal as we can get
> them, a second backend just doubles the maintenance surface without
> making anything people actually use better. See "Why the second backend
> is last" below.

### Short-term — QEMU backend & UX polish

- [x] Network modes (Bridge/Isolated) in `andler-net`
- [x] Host-side bridge creation (via iproute2)
- [x] `--dry-run` flag on `andler create` — prints the resolved config and
      QEMU command line without contacting the daemon at all (client-side
      resolution mirroring the daemon's own logic: OVMF auto-detect, disk
      relocation, `andler_qemu::cmdline::build_args`). Covers TOML mode
      and CLI mode; the interactive wizard already has its own summary
      screen before creating, so `--dry-run` with a bare `andler create`
      isn't supported — see `cli/src/preview.rs`.
- [x] `--verify` flag — validates a resolved instance config (paths
      exist, OVMF found/required-for-Android, disk size sane, GPU
      memory/CPU/memory in range) and prints a ✓/✗ report, without
      contacting the daemon. Exits non-zero if any check fails
      (scriptable). Built on the same client-side resolution as
      `--dry-run` (`preview::resolve_linux`/`resolve_android`) — see
      `cli/src/verify.rs`.
- [x] QEMU backend: improve QMP error handling and recovery — a dropped/
      stale QMP connection no longer stays cached forever (`pause`/
      `resume`/`status` now clear it and reconnect once on a connection-
      level error). New `BackendError::ProcessNotRunning` distinguishes
      "QEMU process itself is gone" (checked via `is_alive()` before
      giving up) from a transient QMP hiccup or a genuine command failure
      (`CommandFailed`/`ParseError`, which are never retried — QEMU
      already answered, retrying changes nothing). See
      `diagnose_and_reset_qmp` in `backends/andler-qemu/src/backend.rs`.
- [x] Core: add disk space pre-check before snapshot operations — checks
      free space on the disk's filesystem via `statvfs(2)` before calling
      `backend.snapshot()`, using guest RAM size as a conservative upper
      bound for vmstate size (exact snapshot size isn't knowable in
      advance). Fails with `DiskError::InsufficientDiskSpace` (mapped to
      `Status::resource_exhausted`) instead of letting the operation run
      out of space partway through. See
      `services/andler-disk/src/diskspace.rs`.
- [x] Core: add VM health checks — periodic background task
      (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s, `0` disables)
      polls every `Running` instance's real backend status; if the
      process has died outside the normal `stop_instance` path, the FSM
      record is transitioned to `Error` and persisted, so a crash is
      visible in `andler status` instead of silently going unnoticed
      until someone happens to check. See
      `daemon/src/daemon/health_ops.rs`.
      **Auto-restart not implemented as an automatic behavior** — but the
      underlying blocker found while implementing this (the FSM had no
      `Start` transition out of `Stopped`/`Error` at all, so *even manual*
      `andler start` didn't work on a stopped/crashed instance) is fixed,
      see the item right below. What's left out is specifically the
      *automatic, unattended* retry-on-crash policy (attempt limits,
      backoff) — a product decision to make deliberately, not bundle in
      silently with a monitoring feature.
- [x] Core: allow restarting a `Stopped`/`Error` instance without
      recreating it — `andler_core::fsm` now accepts `Start` from both
      (returns to `Starting`, same path as a fresh `Created` instance);
      `Daemon::start_instance` needed no changes at all, it was already
      generic over the source state, only the FSM was refusing to let it
      through. `is_terminal()` keeps its old meaning ("this run has
      ended"), not "no transitions remain" — see the updated doc comments
      in `fsm.rs`. `backend.rs::spawn` also now removes a stale QMP socket
      file from a previous run before binding a new one (the deterministic
      per-instance socket path could otherwise collide on restart).

### Medium-term

- [ ] Online Magisk provisioning via guest agent (without `qemu-nbd`)
- [ ] QEMU backend: add hot-plug support for disk/network devices
- [ ] Core: add VM resource limits (CPU pinning, memory overcommit)
- [ ] CLI: add `--export` flag to export VM as OCI container
- [ ] Core: add VM template system for quick VM creation

### Long-term

- [ ] GPU passthrough via VFIO (`RenderBackend::Passthrough`)
- [ ] Tauri GUI client
- [ ] Guest image pipelines (automated Android builds with Waydroid)
- [ ] Live migration between hosts
- [ ] Multi-disk support (snapshot device name parameterization)
- [ ] QMP event subscription (async events beyond command responses)
- [ ] **Cloud Hypervisor backend** (`andler-vmm` with `rust-vmm` crates) —
      deliberately last. Everything above this line makes the existing,
      working QEMU path better for people using it today; a second
      backend is a parallel implementation of `HypervisorBackend` that
      pays for itself only once the first one stops being the bottleneck.

### Why the second backend is last

`andler-vmm` currently exists as an empty stub. Standing up a real Cloud
Hypervisor backend means re-implementing cmdline/process/QMP-equivalent
lifecycle management, metrics, snapshotting, and every edge case the QEMU
backend has already hit — a large, mostly independent effort that doesn't
improve anything for the QEMU path in the meantime. Every item above it
either fixes something that can silently go wrong today (QMP recovery,
disk space pre-checks, health checks) or removes a "just run it and see"
step from the most common workflow (`--dry-run`/`--verify`). Those come
first.
