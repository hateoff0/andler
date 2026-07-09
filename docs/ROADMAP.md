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

### Short-term

- [x] Network modes (Bridge/Isolated) in `andler-net`
- [x] Host-side bridge creation (via iproute2)

### Medium-term

- [ ] Online Magisk provisioning via guest agent (without `qemu-nbd`)
- [ ] QEMU backend: improve QMP error handling and recovery
- [ ] Core: add VM health checks and auto-restart on failure
- [ ] CLI: add --dry-run flag to preview QEMU args before creating
- [ ] CLI: add --verify flag to validate instance config before start
- [ ] Core: add disk space pre-check before snapshot operations
- [ ] QEMU backend: add hot-plug support for disk/network devices
- [ ] Core: add VM resource limits (CPU pinning, memory overcommit)
- [ ] CLI: add --export flag to export VM as OCI container
- [ ] Core: add VM template system for quick VM creation
- [ ] Cloud Hypervisor backend (`andler-vmm` with `rust-vmm` crates)

### Long-term

- [ ] GPU passthrough via VFIO (`RenderBackend::Passthrough`)
- [ ] Tauri GUI client
- [ ] Guest image pipelines (automated Android builds with Waydroid)
- [ ] Live migration between hosts
- [ ] Multi-disk support (snapshot device name parameterization)
- [ ] QMP event subscription (async events beyond command responses)
