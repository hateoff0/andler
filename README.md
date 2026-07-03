# ANDLER

**ANDLER** = *Android Linux Emulator & Runtime*

A Rust daemon and thin CLI client for creating and managing QEMU virtual machines on Linux/KVM. The core value proposition: **full 3D GPU acceleration for Linux and Android guests** via Venus (Vulkan), VirGL (OpenGL), and VFIO passthrough — running graphics-intensive workloads (games, Android apps, Waydroid, development environments) with near-native GPU performance inside VMs.

## What ANDLER Does

ANDLER manages the complete lifecycle of QEMU-based virtual machines with a focus on **GPU-accelerated guests**:

- **Linux guests** with 3D GPU acceleration — install from ISO, get full Vulkan/OpenGL support via Venus/VirGL render backends, use as a daily-driver desktop or development environment
- **Android guests** (Waydroid) — run Android apps with GPU acceleration on Linux hardware, with libndk/libhoudini for ARM→x86 translation
- **Real-time monitoring** — CPU, RAM, disk, network, and GPU metrics (VRAM usage, GPU load) streamed every second
- **Snapshots** — save/restore VM state instantly via QEMU's async job API
- **Clone & export** — duplicate VMs cheaply (linked overlays) or create standalone copies

All managed through a single `andler` CLI or gRPC API, with data stored under `~/.local/share/andler/` (no root required).

## Features

### 3D GPU Acceleration

The primary feature. ANDLER configures QEMU's GPU passthrough and virtualized rendering:

| Backend | How It Works | Best For |
|---------|-------------|----------|
| **Venus** | Vulkan via Venus virtio-gpu protocol | Vulkan-native apps, games, Waydroid |
| **VirGL** | OpenGL over virtio-gpu (virtio-gpu + virglrenderer) | OpenGL apps, desktop compositing |
| **VirtioGpu** | Basic virtio-gpu without 3D acceleration | Lightweight VMs, headless servers |
| **Cpu** | Software rendering (`-vga std`) | CI/CD, headless workloads |
| **Passthrough** | VFIO GPU passthrough (full host GPU to guest) | Maximum performance, gaming |

Venus and VirGL provide **paravirtualized 3D acceleration** — the guest sees a standard GPU driver, and QEMU translates rendering commands to the host GPU. This gives near-native performance for most workloads without requiring GPU passthrough hardware.

### Instance Management

- **Unified `create` command** — Linux/Android VMs from TOML (auto-detected) or CLI flags (`--kind linux|android`)
- **Full lifecycle control** — start, stop, pause, resume, status
- **Factory reset** — `remove --purge` deletes instance files (disk + OVMF vars)
- **Clone & export** — three clone modes (linked, full-standalone, shared-base) for both Linux and Android VMs
- **Default XDG paths** — all data under `~/.local/share/andler/`, no root required

### Snapshots

- **QEMU job-based snapshots** — async create/restore/delete/list via `snapshot-save`/`snapshot-load` job API
- **Per-instance timeout** — configurable `snapshot_timeout_secs` (default 30s)
- **Per-operation override** — `--timeout` flag on snapshot commands
- **SQLite metadata** — tag, description, creation time stored with `ON DELETE CASCADE`

### Monitoring

- **Real-time metrics** — CPU%, RAM, disk I/O, network I/O streamed every second from `/proc`
- **GPU metrics** — AMD (sysfs), NVIDIA (`nvidia-smi`), Intel (i915 sysfs) with automatic vendor detection
- **Live logs** — tail QEMU stdout/stderr in real-time

### Android Support

- **Android profiles** — Android 11/13, GApps, microG, libndk/libhoudini (ARM→x86 translation), root mode selection
- **Overlay disks** — cheap per-instance overlays over shared base image
- **Offline Magisk provisioning** — root access via `qemu-nbd` without booting the VM

### Hypervisor Abstraction

- **`HypervisorBackend` trait** — pluggable backend architecture
- **QEMU backend** — full implementation with QMP control, snapshot API, metrics
- **Cloud Hypervisor stub** — reserved for future `rust-vmm` integration

## Quick Start

### Install

```bash
cargo build --release
```

### Start the Daemon

```bash
./target/release/andlerd
```

Listens on `127.0.0.1:50051` by default. Override with `ANDLERD_ADDR` env var.

### Create a Linux VM (with 3D GPU)

```bash
./target/release/andler create \
  --kind linux \
  --name my-linux \
  --iso-path /path/to/installer.iso \
  --disk-path /path/to/disk.qcow2 \
  --ovmf-vars-template /path/to/VARS.fd
```

After installation, configure GPU acceleration in the TOML:

```toml
[gpu]
render_backend = "Venus"    # or "VirGl" for OpenGL
hostmem_bytes = 4294967296  # 4 GiB VRAM
blob = true
gl = true
```

### Create an Android VM (Waydroid)

```bash
./target/release/andler create \
  --kind android \
  --name my-android \
  --android-version 13 \
  --base-image-path /path/to/base.qcow2 \
  --ovmf-vars-template /path/to/VARS.fd \
  --libndk
```

`--libndk` enables ARM→x86 translation (libhoudini/libndk) for running ARM-only Android apps.

### Create from TOML (auto-detected type)

```bash
# LinuxVm (no android_version field)
./target/release/andler create --file instance.toml

# AndroidVm (has android_version field)
./target/release/andler create --file android.toml
```

### Manage the Instance

```bash
# Start
./target/release/andler start <instance-id>

# Stream metrics (CPU%, RAM, disk, net, GPU)
./target/release/andler metrics <instance-id>

# Tail logs
./target/release/andler logs <instance-id>

# Create a snapshot
./target/release/andler snapshot <instance-id> create --tag before-update

# Stop
./target/release/andler stop <instance-id>

# Restore from snapshot
./target/release/andler snapshot <instance-id> restore --tag before-update

# Remove with file cleanup
./target/release/andler remove <instance-id> --purge
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `create` | Create instance (TOML auto-detect, or `--kind linux`/`--kind android` CLI flags) |
| `start` | Start instance |
| `stop` | Stop instance (`--graceful` for SIGTERM) |
| `pause` | Pause running instance |
| `resume` | Resume paused instance |
| `status` | Print instance state |
| `list` | List all instances |
| `config` | Print full instance configuration |
| `remove` | Remove instance (`--purge` to delete files) |
| `clone` | Clone instance (linked/full-standalone/shared-base) |
| `export` | Export disk as standalone file |
| `logs` | Live-tail QEMU stdout/stderr |
| `metrics` | Stream resource metrics (CPU/RAM/disk/net/GPU) |
| `snapshot` | Snapshot CRUD (create/restore/delete/list) |
| `disk` | Disk management (create/info/resize/compact) |

See [`docs/API.md`](docs/API.md) for full command reference with all flags.

## Configuration

### TOML Instance File

Minimal — only required fields:

```toml
name = "my-vm"
iso_path = "/path/to/installer.iso"
disk_path = "/path/to/disk.qcow2"
ovmf_vars_path = "/path/to/VARS.fd"
```

Optional sections with defaults:

```toml
disk_size_gib = 40
snapshot_timeout_secs = 30

[cpu]
cores = 4
sockets = 1
threads = 1
priority = "Normal"

[memory]
size_bytes = 8589934592  # 8 GiB

[gpu]
render_backend = "Venus"
hostmem_bytes = 4294967296  # 4 GiB
blob = true
gl = true

[display]
resolution = { width = 1920, height = 1080 }
display_engine = "Sdl"

[network]
mode = "Nat"

[audio]
backend = "Pipewire"

[input]
tablet_mode = true
clipboard_enabled = true
```

### Render Backends

| Backend | Description | Status |
|---------|-------------|--------|
| `Venus` | Vulkan via Venus (virtio-gpu) | Implemented |
| `VirtioGpu` | VirtIO-GPU without Venus | Implemented |
| `VirGl` | VirGL (OpenGL over virtio-gpu) | Implemented |
| `Cpu` | Software rendering (`-vga std`) | Implemented |
| `Passthrough` | VFIO GPU passthrough | Not implemented |

### Display Engines

| Engine | Description |
|--------|-------------|
| `Sdl` | Direct SDL window |
| `Spice` | SPICE server for remote GUI |
| `Dbus` | D-Bus compositor integration |
| `None` | Headless (`-display none`) |

### Network Modes

| Mode | Description |
|------|-------------|
| `Nat` | QEMU user-mode networking (default) |
| `Bridge` | Connect to host bridge interface |
| `Isolated` | No network connectivity |

### Root Modes

| Mode | Description |
|------|-------------|
| `None` | No root access |
| `Magisk` | Offline Magisk provisioning (requires `--magisk-dir`) |

## Repository Structure

```
andler/
├── core/                          Domain types, backend trait, config, FSM
│   └── andler-core/               22 unit tests, no external dependencies
│       ├── lib.rs                  Re-exports
│       ├── error.rs                BackendError, FsmError
│       ├── backend.rs              HypervisorBackend trait, BackendHandle, BackendKind
│       ├── fsm.rs                  InstanceState enum (FSM transitions)
│       ├── clone.rs                CloneMode enum (Linked, FullStandalone, SharedBase)
│       ├── android_profile.rs      AndroidProfile, AndroidVersion
│       └── config/
│           ├── mod.rs              InstanceConfig, InstanceId, InstanceKind
│           ├── instance.rs         InstanceKind (LinuxVm, AndroidVm)
│           ├── cpu.rs              CpuConfig (cores, sockets, threads, affinity, priority)
│           ├── memory.rs           MemoryConfig (size_bytes)
│           ├── disk.rs             DiskConfig (path, format, base_image, snapshot_timeout_secs)
│           ├── gpu.rs              GpuConfig (render_backend, gpu_type)
│           ├── display.rs          DisplayConfig (headless, width, height, bpp)
│           ├── network.rs          NetworkConfig, NetworkMode (None, Bridge, Isolated)
│           ├── firmware.rs         FirmwareConfig (bios, ovmf_vars)
│           ├── audio.rs            AudioConfig (backend)
│           └── input.rs            InputConfig (keyboard, mouse)
│
├── backends/                      Hypervisor implementations
│   ├── andler-qemu/               QEMU backend
│   │   ├── cmdline.rs             QEMU CLI argument builder
│   │   ├── process.rs             Process spawn/terminate/logs
│   │   ├── qmp.rs                 QMP protocol client
│   │   ├── backend.rs             HypervisorBackend implementation
│   │   └── metrics.rs             /proc-based per-VM resource metrics
│   └── andler-vmm/                Stub for future Cloud Hypervisor
│
├── services/                      Infrastructure services
│   ├── andler-disk/               qemu-img wrapper + Magisk provisioning
│   ├── andler-net/                Stub for networking
│   ├── andler-store/              SQLite state persistence
│   ├── andler-firmware/           OVMF detect/provision + host hardware
│   │                              auto-detect (GPU/ARM/audio/passt) +
│   │                              host-level GPU metrics (AMD/NVIDIA/Intel)
│   └── andler-rpc/                gRPC protocol + conversions
│
├── daemon/                        Background service (80 unit + 25 integration tests)
│   └── src/
│       ├── daemon/
│       │   ├── mod.rs             Core orchestration logic (~250 lines)
│       │   ├── error.rs           DaemonError enum (14 variants)
│       │   ├── types.rs           InstanceRecord, SnapshotRecord, InstanceDirGuard
│       │   ├── instance_ops.rs    create/start/stop/pause/resume/remove
│       │   ├── clone_ops.rs       clone_instance, export, find_live_clones
│       │   ├── snapshot_ops.rs    create/restore/delete/list snapshots
│       │   ├── query_ops.rs       status, list, get_config, stream
│       │   └── tests/             80 unit tests across 8 modules
│       ├── service.rs             gRPC service wrapper
│       └── grpc_roundtrip_test.rs Integration tests
│
├── cli/                           Command-line client
│   └── src/
│       ├── main.rs                CLI dispatch + enums (431 lines)
│       ├── instance_file.rs       TOML parser (452 lines)
│       ├── create.rs              Create command
│       ├── status.rs              Status, List, Config, Logs, Metrics
│       ├── snapshot.rs            Snapshot commands
│       ├── disk.rs                Disk commands
│       ├── lifecycle.rs           Start, Stop, Pause, Resume, Remove
│       ├── clone.rs               Clone, Export
│       └── helpers.rs             parse_size, format_size, format_bytes
│
├── docker/                        Build & test infrastructure
├── docs/                          Project documentation
└── scripts/                       Development helpers
```

## Default Paths

All data under `~/.local/share/andler/` (XDG data directory):

```
~/.local/share/andler/
├── state.db                    SQLite state store
├── instances/
│   └── <uuid>/
│       ├── instance.toml       Instance configuration
│       ├── disk.qcow2          Instance disk (or overlay)
│       └── VARS.fd             Per-instance OVMF vars copy
└── images/                     Base images (future)
```

## Building

### Local

```bash
cargo build --workspace
cargo test --workspace
```

Requires `/dev/kvm` (user in `kvm` group) for integration tests. Unit tests do not depend on KVM.

### Docker

```bash
# Unit tests (no KVM required)
docker compose -f docker/docker-compose.yml build --no-cache unit-test
docker compose -f docker/docker-compose.yml run --rm unit-test

# E2E smoke test (requires KVM)
docker compose -f docker/docker-compose.yml build --no-cache e2e
docker compose -f docker/docker-compose.yml run --rm e2e
```

## Documentation

| Document | Description |
|----------|-------------|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | System design, crate responsibilities, data flow, FSM diagram |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | Build, test, and development workflows |
| [`docs/API.md`](docs/API.md) | CLI commands, TOML config format, gRPC protocol |
| [`docs/CHANGELOG.md`](docs/CHANGELOG.md) | Feature history by version |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Short/medium/long-term development plans |

### Crate Documentation

Each crate has its own README with detailed API reference:

- [`core/andler-core/README.md`](core/andler-core/README.md) — Domain types, 27 public types, 22 tests
- [`backends/andler-qemu/README.md`](backends/andler-qemu/README.md) — QEMU backend, 50 tests
- [`backends/andler-vmm/README.md`](backends/andler-vmm/README.md) — Cloud Hypervisor stub
- [`services/andler-disk/README.md`](services/andler-disk/README.md) — Disk ops + Magisk provisioning
- [`services/andler-net/README.md`](services/andler-net/README.md) — Networking stub
- [`services/andler-store/README.md`](services/andler-store/README.md) — SQLite persistence
- [`services/andler-rpc/README.md`](services/andler-rpc/README.md) — gRPC protocol + conversions
- [`daemon/README.md`](daemon/README.md) — Daemon orchestration, 80 unit tests + 25 integration tests
- [`cli/README.md`](cli/README.md) — CLI commands + TOML parser

## Metrics

Real-time resource monitoring from host `/proc` (no QMP required):

```
  CPU%   RAM        Disk R     Disk W     Net RX     Net TX     VRAM       GPU%
 12.34   1.23 GiB   45.6 MB/s  12.3 MB/s  1.2 MB/s   0.5 MB/s   512 MiB    67.8%
```

| Metric | Source | Calculation |
|--------|--------|-------------|
| CPU% | `/proc/<pid>/stat` | Delta-based utime+stime / uptime / num_cpus |
| RAM | `/proc/<pid>/status` | VmRSS direct read |
| Disk I/O | `/sys/block/<dev>/stat` | Delta-based bytes/sec |
| Network I/O | `/proc/<net/dev>` | Delta-based bytes/sec |
| VRAM | AMD sysfs / NVIDIA nvidia-smi / Intel sysfs | Vendor-specific |
| GPU Load | AMD sysfs / NVIDIA nvidia-smi / Intel busyiffies delta | Vendor-specific |

Polling interval: 1 second. GPU metrics: AMD → NVIDIA → Intel (first found vendor wins).

## Requirements

- Linux with KVM (`/dev/kvm`)
- QEMU with OVMF/UEFI support
- Rust stable (via rustup)
- Docker + Docker Compose (for reproducible builds)
- `nbd` kernel module + `qemu-nbd` (for Magisk provisioning)
- `protobuf-compiler` (`protoc`) for gRPC code generation

## Roadmap

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for full roadmap with short/medium/long-term plans.

## License

GPL-3.0
