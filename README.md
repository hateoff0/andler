# ANDLER

**ANDLER** = *Android Linux Emulator & Runtime*

A Rust daemon and thin CLI client for creating and managing QEMU virtual machines on Linux/KVM. The core value proposition: **full 3D GPU acceleration for Linux and Android guests** via Venus (Vulkan), VirGL (OpenGL), and VFIO passthrough — running graphics-intensive workloads (games, Android apps, Waydroid, development environments) with near-native GPU performance inside VMs.

## What ANDLER Does

ANDLER manages the complete lifecycle of QEMU-based virtual machines with a focus on **GPU-accelerated guests**:

- **Linux guests** with 3D GPU acceleration — install from ISO, get full Vulkan/OpenGL support via Venus/VirGL render backends, use as a daily-driver desktop or development environment
- **Android guests** (Waydroid) — run Android apps with GPU acceleration on Linux hardware, with libndk/libhoudini for ARM→x86 translation
- **Default XDG paths** — all data under `~/.andler/`, no root required for normal operation (offline guest ops go through one privileged helper binary `sudo -n andler-helper`, set up by `andler doctor --fix`)
- **Real-time monitoring** — CPU, RAM, disk, network, and GPU metrics (VRAM usage, GPU load) streamed every second
- **Snapshots** — disk-only internal qcow2 snapshots: create/delete live on any GPU/audio/CPU configuration, restore offline with a stopped instance
- **Clone & export** — duplicate VMs cheaply (linked overlays) or create standalone copies

All managed through a single `andler` CLI or gRPC API, with data stored under `~/.andler/` (no root required).

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
- **Default XDG paths** — all data under `~/.andler/`, no root required

### Snapshots

- **Disk-only snapshots** — create/delete live via QMP (`blockdev-snapshot-internal-sync`) on any GPU/audio/CPU configuration; restore offline via `qemu-img snapshot -a` (requires stopped instance)
- **SQLite metadata** — tag, description, creation time stored with `ON DELETE CASCADE`

### Monitoring

- **Real-time metrics** — CPU%, RAM, disk I/O, network I/O streamed every second from `/proc`
- **GPU metrics** — AMD (sysfs), NVIDIA (NVML + `nvidia-smi` fallback), Intel (i915 sysfs) with automatic vendor detection
- **Live logs** — tail QEMU stdout/stderr in real-time

### Android Support

- **Android profiles** — Android 11/13, GApps, microG, libndk/libhoudini (ARM→x86 translation)
- **Overlay disks** — cheap per-instance overlays over shared base image

### Hypervisor Abstraction

- **`HypervisorBackend` trait** — pluggable backend architecture
- **QEMU backend** — full implementation with QMP control, snapshot API, metrics

## Quick Start

### Install

```bash
cargo build --release
```

### Start the Daemon

```bash
./target/release/andlerd
```

Listens on `127.0.0.1:50051` by default. Override with `ANDLERD_LISTEN_ADDR` env var (the client connects via `ANDLERD_ADDR` or `--daemon-addr`).

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
  --arm-translator libndk
```

`--arm-translator libndk` enables ARM→x86 translation (libndk for AMD CPUs, libhoudini for Intel CPUs) for running ARM-only Android apps.

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
./target/release/andler snapshot create <instance-id> --tag before-update

# List known guest packages and their status
./target/release/andler guest list <instance-id>

# Stop
./target/release/andler stop <instance-id>

# Restore from snapshot
./target/release/andler snapshot restore <instance-id> --tag before-update

# Remove with file cleanup
./target/release/andler remove <instance-id> --purge
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `create` | Create instance (TOML auto-detect, or `--kind linux`/`--kind android` CLI flags) |
| `wizard` | Launch interactive wizard (default when `andler` is invoked without a subcommand) |
| `config` | View/edit/set instance config (`config view`, `config edit` opens `instance.toml` in `$VISUAL`/`$EDITOR`) |
| `start` | Start instance |
| `stop` | Stop instance (default: force kill (SIGKILL); `--graceful`: graceful ACPI shutdown (SIGTERM)) |
| `pause` | Pause running instance |
| `resume` | Resume paused instance |
| `list` | List all instances (`--state`, `--name` regex, `--sort`, `--json`) |
| `remove` | Remove instance (`--purge` to delete files) |
| `clone` | Clone instance (linked/full-standalone/shared-base) |
| `export` | Export disk as standalone file |
| `logs` | Live-tail QEMU stdout/stderr (`--source`, `--grep` regex, `--tail`) |
| `metrics` | Stream resource metrics (`--once` for single sample, `--json`) |
| `snapshot` | Snapshot CRUD (create/restore/delete/list) |
| `disk` | Disk management (create/info/resize `--shrink`/compact) |
| `guest` | Guest package management (install/remove/list). Auto-fallback: online via the QGA guest-agent socket if running, offline via qemu-nbd if stopped. |
| `completions` | Generate shell completion script (bash/zsh/fish) |
| `doctor` | Check local environment (KVM, QEMU, OVMF, nbd, andler-helper + sudoers rule, andlerd, base images). `--fix` installs the helper and writes/migrates its single sudoers rule. |

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
disk_size_gib = 256
snapshot_timeout_secs = 30   # accepted for compatibility; snapshot ops are synchronous
compact_on_shutdown = false

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
device_model = "virtio-net-pci"
nat_backend = "Slirp"  # or "Passt" if available

[audio]
backend = "Pipewire"
device = "VirtioSound"

[input]
pointer_mode = "Tablet"
hide_host_cursor = true
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
| `Sdl` | Direct SDL window (default on NVIDIA) |
| `Gtk` | GTK window with built-in QEMU UI (default on non-NVIDIA hosts) |
| `Spice` | SPICE server for remote GUI |
| `Dbus` | D-Bus compositor integration |
| `None` | Headless (`-display none`) |

Note: the configured `resolution` **is applied inside the guest OS**: on every boot the guest applies it from fw_cfg (`opt/andler/display-resolution`, oneshot `andler-display-resolution.service` in the base image), and `andler config set <id> display.resolution WxH` on a running instance applies it live through the guest agent (Android: compositor restart; Linux: `andler-apply-resolution`). Host-side `Sdl`/`Gtk` windows size themselves to the guest framebuffer.

### Network Modes

| Mode | Description |
|------|-------------|
| `Nat` | QEMU user-mode networking (default) |
| `Bridge` | Connect to host bridge interface |
| `Isolated` | No network connectivity |

### Clipboard Sharing

`clipboard_enabled = true` sets up the host side correctly (a `virtio-serial`
`qemu-vdagent` chardev), but clipboard sharing only actually works once the
**guest** OS has `spice-vdagent` installed and running — this is not something
ANDLER can install or detect from the host side. If clipboard doesn't work
after boot, install it inside the guest:

```bash
andler guest install spice-vdagent <instance-id>
```

## Repository Structure

```
andler/
├── core/                          Domain types, backend trait, config, FSM
│   └── andler-core/               ~50 unit tests, no external dependencies
│       ├── lib.rs                  Re-exports
│       ├── error.rs                BackendError, FsmError
│       ├── backend.rs              HypervisorBackend trait, BackendHandle, BackendStatus, ResourceMetrics
│       ├── fsm.rs                  InstanceState enum (FSM transitions)
│       ├── clone.rs                CloneMode enum (Linked, FullStandalone, SharedBase)
│       ├── android_profile.rs      AndroidProfile, AndroidVersion, ArmTranslator
│       ├── paths.rs                Unified path resolution (ANDLER_HOME, runtime_dir, ensure_private_dir)
│       └── config/
│           ├── mod.rs              InstanceConfig, InstanceId, InstanceKind
│           ├── instance.rs         InstanceKind (LinuxVm, AndroidVm), BackendKind
│           ├── cpu.rs              CpuConfig (cores, sockets, threads, affinity, priority)
│           ├── memory.rs           MemoryConfig (size_bytes, ballooning, zram, ksm)
│           ├── disk.rs             DiskConfig (path, format, base_image, compact_on_shutdown)
│           ├── gpu.rs              GpuConfig (render_backend, hostmem_bytes, blob, gl)
│           ├── display.rs          DisplayConfig (resolution, dpi, fps_limit, display_engine, fullscreen)
│           ├── network.rs          NetworkConfig, NetworkMode, NatBackend (Slirp, Passt)
│           ├── firmware.rs         FirmwareConfig (ovmf_code_path, ovmf_vars_path)
│           ├── audio.rs            AudioConfig (backend, device)
│           ├── input.rs            InputConfig (pointer_mode, hide_host_cursor, clipboard_enabled)
│           └── cdrom.rs            CdromBus enum (VirtioScsi, Ide)
│
├── backends/                      Hypervisor implementations
│   ├── andler-qemu/               QEMU backend
│   │   ├── cmdline.rs             QEMU CLI argument builder
│   │   ├── process.rs             Process spawn/terminate/logs
│   │   ├── qmp.rs                 QMP protocol client
│   │   ├── backend.rs             HypervisorBackend implementation
│   │   └── metrics.rs             /proc-based per-VM resource metrics
│
├── services/                      Infrastructure services
│   ├── andler-disk/               qemu-img wrapper + guest tools offline provisioning
│   ├── andler-net/                Bridge/NAT networking via iproute2 (isolated: config-only)
│   ├── andler-store/              SQLite state persistence
│   ├── andler-firmware/           OVMF detect/provision + host hardware
│   │                              auto-detect (GPU/ARM/audio/passt) +
│   │                              host-level GPU metrics (AMD/NVIDIA/Intel)
│   └── andler-rpc/                gRPC protocol + conversions
│
├── apps/                          User-facing applications
│   ├── daemon/                    Background service (~120 unit tests, incl. gRPC round-trip)
│   │   └── src/
│   │       ├── main.rs            andlerd binary (verbosity flags, signal handling)
│   │       ├── firmware.rs        OVMF auto-detection
│   │       ├── service.rs         gRPC service wrapper
│   │       ├── grpc_roundtrip_test.rs  Integration tests
│   │       └── daemon/
│   │           ├── mod.rs         Core orchestration logic (~268 lines)
│   │           ├── error.rs       DaemonError enum (29 variants)
│   │           ├── types.rs       InstanceRecord, SnapshotRecord, InstanceDirGuard
│   │           ├── instance_ops.rs  create/start/stop/pause/resume/remove, resolve_instance_id
│   │           ├── clone_ops.rs   clone_instance, export, find_live_clones
│   │           ├── snapshot_ops.rs  create/restore/delete/list snapshots
│   │           ├── query_ops.rs   status, list, get_config, update_instance_config, stream
│   │           ├── health_ops.rs  health check, mark_instance_crashed
│   │           └── tests/         12 test modules
│   │
│   └── cli/                       Command-line client
│       └── src/
│           ├── main.rs            CLI dispatch + enums
│           ├── instance_file.rs   TOML parser
│           ├── create.rs          Create command
│           ├── status.rs          Status, List (--full-id/-q), Config, Logs, Metrics
│           ├── snapshot.rs        Snapshot commands
│           ├── disk.rs            Disk commands (create/info/resize/compact)
│           ├── lifecycle.rs       Start, Stop, Pause, Resume, Remove
│           ├── clone.rs           Clone, Export
│           ├── helpers.rs         parse_size, format_size, ensure_qcow2_extension
│           ├── guest.rs           Guest subcommand (package management via guest agent)
│           ├── doctor.rs          Doctor subcommand (environment checks)
│           ├── edit.rs            config edit ($VISUAL/$EDITOR on real instance.toml)
│           ├── verify.rs          --verify flag (pre-flight checks)
│           ├── preview.rs         --dry-run flag (resolve QEMU command line)
│           └── wizard/            Interactive setup wizard
│               ├── mod.rs         Entry point, orchestration, build_create_request()
│               ├── basic.rs       Basic mode (6 questions)
│               ├── advanced.rs    Advanced mode (15 questions)
│               └── summary.rs     Summary with Create/Modify/Cancel
│
├── docker/                        Build & test infrastructure
├── docs/                          Project documentation
└── scripts/                       Development helpers
```

## Default Paths

All data under `~/.andler/`:

```
~/.andler/
├── andlerd.db                   SQLite state store
├── instances/
│   └── <id>/
│       ├── instance.toml       Instance configuration
│       ├── disk.qcow2          Instance disk (or overlay)
│       └── VARS.fd             Per-instance OVMF vars copy
└── cache/                      
    └── base-images/            Base images for Android instances
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
# Unit tests (no KVM required; incremental thanks to BuildKit cache mounts)
docker compose -f docker/e2e/compose.yaml build unit-test
docker compose -f docker/e2e/compose.yaml run --rm unit-test

# E2E suite (requires KVM; covers all CLI commands)
docker compose -f docker/e2e/compose.yaml build e2e
docker compose -f docker/e2e/compose.yaml run --rm e2e
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

- [`core/andler-core/README.md`](core/andler-core/README.md) — Domain types, ~30 public types, ~50 tests
- [`backends/andler-qemu/README.md`](backends/andler-qemu/README.md) — QEMU backend, ~80 tests
- [`services/andler-disk/README.md`](services/andler-disk/README.md) — Disk ops + guest tools provisioning
- [`services/andler-store/README.md`](services/andler-store/README.md) — SQLite persistence
- [`services/andler-net/README.md`](services/andler-net/README.md) — Bridge/NAT networking via iproute2 (isolated: config-only)
- [`services/andler-firmware/README.md`](services/andler-firmware/README.md) — Hardware auto-detection + GPU metrics
- [`services/andler-rpc/README.md`](services/andler-rpc/README.md) — gRPC protocol and conversions
- [`apps/daemon/README.md`](apps/daemon/README.md) — Daemon orchestration, ~120 unit tests (incl. gRPC round-trip)
- [`apps/cli/README.md`](apps/cli/README.md) — CLI commands + TOML parser + wizard, ~120 tests

## Metrics

Real-time resource monitoring from host `/proc` (no QMP required):

```
cpu=12.3%    rss=1.23 GiB   disk_r=45.6 MB/s  disk_w=12.3 MB/s  net_rx=1.2 MB/s   net_tx=0.5 MB/s   vram=512 MiB       gpu=68%
```

| Metric | Source | Calculation |
|--------|--------|-------------|
| CPU% | `/proc/<pid>/stat` | Delta-based utime+stime / uptime / num_cpus |
| RAM | `/proc/<pid>/status` | VmRSS direct read |
| Disk I/O | `/proc/<pid>/io` | read_bytes/write_bytes delta |
| Network I/O | `/proc/<net/dev>` | Delta-based bytes/sec |
| VRAM | AMD sysfs / NVIDIA NVML (`nvml-wrapper`) / Intel sysfs | Vendor-specific |
| GPU Load | AMD sysfs / NVIDIA NVML + nvidia-smi fallback / Intel rc6_residency_ms delta | Vendor-specific |

Polling interval: 1 second. GPU metrics: AMD → NVIDIA → Intel (first found vendor wins).

## Requirements

- Linux with KVM (`/dev/kvm`)
- QEMU with OVMF/UEFI support
- `nbd` kernel module, for offline guest operations (`andler guest install/remove/boot-mode`
  on a stopped instance mounts its disk via `qemu-nbd`). `andlerd` tries to auto-load it
  itself the first time it's needed (`sudo -n modprobe nbd max_part=8`) — this only works
  if you've added a matching sudoers rule (below); otherwise load it manually:
  ```bash
  sudo modprobe nbd max_part=8
  # to persist across reboots:
  echo nbd | sudo tee /etc/modules-load.d/nbd.conf
  ```
  These same offline operations also need root to open `/dev/nbd*`, to `mount`/`umount`
  the guest partition, and to `chroot` into it for package management — `andlerd` runs
  unprivileged and shells out to `sudo -n` **only** for a single privileged helper
  binary (`/usr/local/sbin/andler-helper`, installed as root:root 0755 by
  `andler doctor --fix`), rather than granting passwordless sudo to ten system tools.
  The helper re-validates every argument it receives (paths must lie inside the
  NBD-mounted guest partition — the `file cp-a` source is the one exception, a
  read-only host path from the translator cache — devices must be free
  `/dev/nbd*` devices, chroot commands come from a strict allowlist) before
  doing anything. Without the
  passwordless rule below these operations will fail with a permission error;
  add it via `andler doctor --fix` or `sudo visudo`:
  ```
  youruser ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper
  ```
  `andler doctor --fix` installs the helper binary (root:root 0755) and migrates
  any legacy per-binary rules from the old sudoers format automatically.
- Rust stable (via rustup)
- Docker + Docker Compose (for reproducible builds)
- `protobuf-compiler` (`protoc`) for gRPC code generation

## Roadmap

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for full roadmap with short/medium/long-term plans.

## Credits & Acknowledgments

Thanks to the projects ANDLER builds on. Titles link to the source of truth.

### Virtualization stack

- [QEMU](https://www.qemu.org/) — hypervisor; `qemu-img`/`qemu-nbd` power disk ops
- [KVM](https://www.kvm.org/) — Linux kernel virtualization (requires `/dev/kvm`)
- [virtio](https://docs.oasis-open.org/virtio/) — paravirtualized devices
  (virtio-blk/net/gpu/serial/sound)
- [Venus](https://docs.mesa3d.org/drivers/venus.html) — Vulkan over virtio-gpu
- [VirGL](https://docs.mesa3d.org/drivers/virgl.html) — OpenGL over virtio-gpu
- [OVMF / edk2](https://github.com/tianocore/edk2) — UEFI firmware for guest boot

### Android ecosystem

- [Waydroid](https://github.com/waydroid/waydroid) — Android-in-container
  runtime: overlay layout, boot-mode switching, ARM translation target
- [waydroid_script (casualsnek)](https://github.com/casualsnek/waydroid_script)
  — reference ARM translation implementation (pins, props, binfmt)
- [waydroid-helper](https://github.com/waydroid-helper/waydroid-helper) —
  reference translator packaging; our `houdini.rc` is byte-identical to its
  reference file
- [libndk_translation (Google)](https://github.com/supremegamers/vendor_google_proprietary_ndk_translation-prebuilt)
  — ARM→x86 translation runtime (prebuilt mirror used for installs)
- [libhoudini (Intel)](https://github.com/supremegamers/vendor_intel_proprietary_houdini)
  — ARM→x86 translation runtime (prebuilt mirror used for installs)
- [binfmt_misc (kernel)](https://docs.kernel.org/admin-guide/binfmt-misc.html)
  — dispatches ARM ELF binaries to the translation runtime

### Tooling & architecture references

- [Docker / Docker Compose](https://docs.docker.com/) — containerized test
  harness (`docker/e2e/`)
- [Proxmox VE](https://github.com/proxmox/proxmox-rs) — Rust daemon/API
  reference for improving daemon quality: `pvedaemon` (root daemon,
  localhost-only API, thin client — same security model as andlerd),
  `proxmox-api-server`/`proxmox-router` (typed parameter validation,
  error→status mapping)
- [containerd](https://github.com/containerd/containerd) — gRPC daemon
  architecture reference: plugin services, snapshots, event streaming,
  thin `ctr` client
- [tokio](https://github.com/tokio-rs/tokio) · [tonic/prost](https://github.com/hyperium/tonic) ·
  [clap](https://github.com/clap-rs/clap) · [rusqlite](https://github.com/rusqlite/rusqlite) ·
  [serde](https://github.com/serde-rs/serde) · [tracing](https://github.com/tokio-rs/tracing) ·
  [thiserror](https://github.com/dtolnay/thiserror) — Rust ecosystem

## License

GPL-3.0
