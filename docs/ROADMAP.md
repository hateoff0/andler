# Roadmap

## Done

- [x] Domain model and backend abstraction (`andler-core`)
- [x] QEMU backend with full lifecycle management (`andler-qemu`)
- [x] QEMU VM snapshots — create/restore/delete/list via job API
- [x] Real-time resource metrics from `/proc` (CPU, RAM, disk, network)
- [x] AMD GPU metrics from sysfs (VRAM, GPU load)
- [x] NVIDIA GPU metrics via `nvidia-smi` CLI (VRAM, GPU load)
- [x] Intel GPU metrics via sysfs `i915` (GPU load delta, VRAM stolen memory)
- [x] Offline Magisk provisioning via `qemu-nbd`
- [x] SQLite state persistence with cascade delete (`andler-store`)
- [x] gRPC protocol with 19 RPCs and bidirectional conversions
- [x] Factory reset with file cleanup (`remove --purge`)
- [x] Clone/export for Linux and Android VMs (3 modes)
- [x] Configurable per-instance snapshot timeout
- [x] Snapshot timeout per-operation override (`--timeout` flag)
- [x] Unified `create` command with `--kind linux`/`--kind android` + Android TOML
- [x] Monorepo restructure (core/backends/services/daemon/cli)
- [x] Comprehensive documentation in English

## In Progress

- (none currently)

## Planned

### Short-term

- [ ] Network modes (Bridge/Isolated) in `andler-net`
- [ ] Host-side bridge creation and nftables rules

### Medium-term

- [ ] GPU passthrough via VFIO (`RenderBackend::Passthrough`)
- [ ] Cloud Hypervisor backend (`andler-vmm` with `rust-vmm` crates)
- [ ] Online Magisk provisioning via guest agent (without `qemu-nbd`)

### Long-term

- [ ] Tauri GUI client
- [ ] Guest image pipelines (automated Android builds with Waydroid)
- [ ] Guest agent integration (host↔guest communication)
- [ ] Live migration between hosts
- [ ] Multi-disk support (snapshot device name parameterization)
- [ ] QMP event subscription (async events beyond command responses)
