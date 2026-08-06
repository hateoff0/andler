# Architecture

ANDLER (**ANDLER** = *Android Linux Emulator & Runtime*) is a Rust monorepo for running and managing virtual machines (Android and Linux guests) on Linux/KVM. It follows a daemon + thin CLI architecture, with QEMU as the current hypervisor backend and gRPC as the communication protocol.

## Design Principles

1. **Domain-driven separation**: Pure domain types in `andler-core` with no infrastructure dependencies. All I/O (QEMU, filesystem, network, database) lives in separate service/backend crates.
2. **Backend abstraction**: The `HypervisorBackend` trait defines a hypervisor-agnostic interface. Adding a new hypervisor means implementing this trait — no changes to daemon, CLI, or RPC.
3. **Explicit state machine**: Instance lifecycle is governed by a strict FSM with named states and validated transitions. No implicit state changes.
4. **Persistence is optional**: The daemon works with or without SQLite persistence. In-memory state is always the source of truth for the current session. Store errors are logged but don't fail operations.
5. **No global state**: Each crate has clear responsibilities. `andler-core` knows nothing about QEMU, gRPC, or SQL. `andler-qemu` knows nothing about the daemon or CLI.
6. **User-mode by default**: All paths under `~/.andler/`. No root/sudo required for normal operation.

## Crate Dependency Graph

```
andler-core          (no workspace dependencies — bottom layer)
    ↑
    ├── andler-qemu     (depends on: core)
    ├── andler-vmm      (depends on: core)
    ├── andler-disk     (depends on: core types only via error, mostly standalone)
    ├── andler-net      (depends on: core)
    ├── andler-store    (depends on: core)
    ├── andler-firmware (depends on: no workspace crates — standalone, uses sysfs/nvml)
    └── andler-rpc      (depends on: core)
          ↑
    andler-daemon    (depends on: core, qemu, firmware, disk, store, rpc)
          ↑
    andler-cli       (depends on: rpc only — thin client)
```

## Crates in Detail

### `core/andler-core` — Domain Model

The foundation crate. Defines all public types that other crates depend on.

**Key types:**
- `HypervisorBackend` trait: The contract for all hypervisor implementations (17 methods)
- `InstanceConfig`: 9-section configuration struct (disk, cpu, memory, display, gpu, network, audio, input, firmware)
- `InstanceState` / `InstanceEvent`: FSM with 7 states and 7 events
- `ResourceMetrics`: All-Optional metrics struct (CPU, RAM, disk I/O, net I/O, GPU)
- `CloneMode`: `Linked` | `FullStandalone` | `SharedBase`
- `AndroidProfile`: Android version + root mode + app store configuration
- `BackendError` / `FsmError`: Domain error types
- `paths`: Unified path resolution (`runtime_dir()`, `current_uid()`, `ensure_private_dir()`)

**~50 unit tests**, fully testable without QEMU or `/dev/kvm`.

**Must not depend on any other workspace crate.**

### `backends/andler-qemu` — QEMU Backend

Implements `HypervisorBackend` for QEMU via process management, QMP communication, and `/proc`-based metrics.

**Key components:**
- `cmdline.rs`: Pure function translating `InstanceConfig` to QEMU CLI arguments (argument blocks, each tested independently against reference configuration). Includes the guest-agent block: a `socket` chardev (`*.qga.sock`, `server=on,wait=off`) wired to a `virtserialport` named `org.qemu.guest_agent.0`, plus a `-fw_cfg name=opt/andler/display-resolution,string=WxH` entry when a resolution is configured
- `process.rs`: `QemuProcess` — spawn, terminate, force-kill, log/metrics broadcast channels, `qga_socket_path()`
- `qmp.rs`: `QmpClient` — QMP protocol over unix socket (handshake, pause/resume/status, snapshot job API) **and** the QEMU guest agent (QGA) protocol via `connect_agent()` on the `*.qga.sock` chardev (`guest-ping`/`guest-exec`/`guest-exec-status`/`guest-file-open`/...)
- `backend.rs`: `QemuBackend` — ties everything together, manages `RunningInstance` registry; guest operations (`guest_exec_package`, `set_guest_display_resolution`, ...) talk to the agent chardev socket, **not** the QMP monitor (QEMU ≥ 9 no longer registers `guest-*` commands on QMP)
- `metrics.rs`: Background poller reading `/proc/<pid>/stat`, `/proc/<pid>/status`, `/sys/block/*/stat`, `/proc/<net/dev` (per-VM); calls into `andler-firmware::metrics` for the GPU fields (host-level, not per-VM — moved there to sit next to GPU vendor detection). Sample collection runs in `tokio::task::spawn_blocking` — the `/proc` and sysfs reads are synchronous I/O and must not block the async runtime.

**Tests**: unit + integration, across `cmdline`/`process`/`qmp`/`backend`/`metrics` (GPU metrics tests moved to `services/andler-firmware`).

### `backends/andler-vmm` — Cloud Hypervisor (Stub)

Empty stub. Returns `NotImplemented` for all methods. Reserved for future `rust-vmm` / Cloud Hypervisor integration.

### `services/andler-disk` — Disk Operations

Wrapper around `qemu-img` for disk creation/cloning/resizing, plus guest tools offline provisioning.

**Key components:**
- `qcow2.rs`: 7 async functions wrapping `qemu-img` CLI
- `overlay.rs`: Android-specific overlay disk creation + factory reset
- `clone.rs`: 3 clone modes (linked, full-standalone, shared-base)
- `nbd.rs`: nbd device management with flock-based locking, `nbd_status()`, and the chroot environment setup (`bind_host_mounts`): the guest's `/etc/resolv.conf` is *written* with the host's nameservers (via NOPASSWD `sudo chroot` — a dangling symlink would make a bind-mount fail with ENOENT), `/dev`, `/proc`, `/sys` are bind-mounted, and a fresh tmpfs is mounted on the guest's `/run` (gpg-agent, used by pacman, needs a writable `/run`)
- `guest_tools.rs`: offline guest provisioning via `qemu-nbd` + mount + chroot: detects the package manager (apt-get/dnf/pacman), refreshes package indexes (`update`/`makecache`/`-Sy`) and installs/removes packages inside the chroot
- `arm_translator.rs`: ARM translator package staging in guest images (atomic staging + rename)
- `boot_mode.rs`: Android/Linux boot-mode switching by re-pointing the guest's `default.target` symlink through chroot
- `diskspace.rs`: free-space pre-check before snapshots

**~45 unit tests** (+ ignored integration tests requiring qemu-img).

### `services/andler-net` — Network Configuration

Implements bridge and isolated network modes for QEMU VMs via host-side network configuration.

**Key components:**
- `lib.rs`: `NetworkService` trait and `DefaultNetworkService` implementation using `iproute2` for bridge setup/teardown. `setup_isolated` is **not implemented** yet (returns an explicit `SetupFailed("isolated network mode is not implemented yet")` error) — the config type exists, the network setup does not.

### `services/andler-firmware` — Firmware & Hardware Detection

Standalone crate for firmware discovery, hardware auto-detection, and GPU metrics. No workspace dependencies — communicates with hardware via sysfs and vendor CLIs.

**Key components:**
- `detect/`: Hardware auto-detection (GPU, OVMF, ARM translator, audio, network passt)
- `metrics/`: GPU metrics collection (NVIDIA via NVML + nvidia-smi fallback, AMD via sysfs, Intel via i915 delta)
- `HardwareDefaults` struct: `detect_all()` returns detected hardware for wizard defaults

**~50 tests** across `detect/` and `metrics/`.

### `services/andler-store` — SQLite Persistence

Two-table SQLite store with JSON columns.

**Schema:**
- `instances(id TEXT PRIMARY KEY, config_json TEXT, state_json TEXT)`
- `snapshots(id, instance_id, tag, description, created_at)` with `ON DELETE CASCADE`

**~20 tests** using in-memory SQLite.

### `services/andler-rpc` — gRPC Protocol

Protobuf definitions and generated code via `tonic`/`prost`.

**26 RPCs** covering instance lifecycle, monitoring, snapshots, clone/export.
**~45 conversion tests** for bidirectional proto↔domain type mapping.

### `daemon/` — Background Service

Orchestrates all operations. Holds backend registry, instance state, optional persistence.

**Key design:**
- `Daemon::new()` / `with_store()` / `restore()` — three construction paths
- Instance lifecycle via FSM transitions
- `InstanceDirGuard` RAII for cleanup on partial failure
- `create_linux_instance()` / `create_android_instance()` — high-level resource creation + registration
- `resolve_instance_id()` — Docker-style partial ID resolution (8-char hex prefix)
- `update_instance_config()` — Edit config via gRPC, protects id/kind/disk.path
- `DaemonService` — thin gRPC wrapper, one method per Daemon method
- Error mapping: `DaemonError` (29 variants) → gRPC status codes

**~120 unit tests** (some ignored) + **gRPC round-trip tests** (real TCP).

### `cli/` — Command-Line Interface

Thin gRPC client. Each subcommand = one gRPC request + print response.

**Key features:**
- Unified `create` command (TOML for LinuxVm, CLI flags for AndroidVm)
- Interactive wizard with smart defaults and hardware auto-detection
- Real-time metrics streaming with GPU columns (AMD/NVIDIA/Intel)
- Snapshot CRUD with per-operation timeout
- Disk management (create, info, resize with shrink protection, compact)
- Shell completions (bash, zsh, fish)
- Colored status output with `IsTerminal` gating
- `doctor` command for environment diagnostics and auto-fix
**~120 tests** (TOML parsing, helpers, create, wizard, status).

## Data Flow

```
User
  ↓ (CLI command)
andler-cli (gRPC client)
  ↓ (protobuf over TCP)
andler-daemon (gRPC server)
  ↓ (trait methods)
andler-qemu (HypervisorBackend)
  ↓ (process + QMP)
QEMU process → VM
  ↓ (/proc + sysfs)
Metrics poller → broadcast → StreamResourceMetrics → CLI display
  ↓ (SQLite)
andler-store (andlerd.db)
```

## Guest Provisioning (`guest` operations)

Package management and resolution changes inside a guest use a two-tier strategy:

**Online (VM `Running`, guest agent available)**: commands run inside the guest via the QEMU guest agent (QGA) — `guest-exec`/`guest-exec-status` (package install/remove, package-manager detection) and `guest-file-*` (config file writes, e.g. `display.conf` on resolution change). The agent wire runs over the dedicated `*.qga.sock` chardev (`virtserialport name=org.qemu.guest_agent.0`), never the QMP monitor. This requires `qemu-guest-agent` inside the guest, which the base image ships enabled (`qemu-guest-agent.service`).

**Offline path (VM stopped)** — the disk is exposed via `qemu-nbd` and the root partition mounted, then operations run inside a `sudo -n chroot`:

- package manager detected by binary (`apt-get`/`dnf`/`pacman`); package index is refreshed first (`apt-get update` / `dnf makecache` / `pacman -Sy`) so installs work on fresh images
- the chroot is made network- and signature-capable by `bind_host_mounts`: host nameservers are written into the guest's `/etc/resolv.conf` (via chroot, replacing the dangling `stub-resolv.conf` symlink), `/dev`, `/proc`, `/sys` are bind-mounted, and a tmpfs is mounted on the guest's `/run` (gpg-agent needs it for pacman signatures)
- all privileged operations go through `sudo -n` (NOPASSWD rules for `qemu-nbd`, `mount`, `umount`, `chroot`, `modprobe` — see `andler doctor --fix`)

ARM translators (`libndk`/`libhoudini`) use the same offline mount machinery via `switch_arm_translator`: version-keyed download (MD5-verified, cached under `~/.andler/cache/arm-translators/`), staged into `var/lib/waydroid/overlay/system`, `build.prop` updated, old translator removed only after the new one is fully staged.

## Data Flow (guest operations)

```
andler CLI → gRPC → daemon
  ├─ running: backend.qga socket (*.qga.sock) → qemu-ga (guest) → guest-exec/file ops
  └─ stopped: andler-disk → sudo -n qemu-nbd --connect /dev/nbdN → mount → chroot (resolv /dev /proc /sys tmpfs-/-run) → package manager
```

## Instance Lifecycle FSM

```
                    ┌─────────┐
                    │ Created │
                    └────┬────┘
                         │ Start
                         ▼
                    ┌─────────┐
                    │ Starting│
                    └────┬────┘
                         │ StartCompleted
                         ▼
              ┌──── ┌─────────┐ ────┐
         Pause│     │ Running │     │Stop
              │     └────┬────┘     │
              ▼          │          ▼
         ┌─────────┐     │     ┌─────────┐
         │ Paused  │     │     │Stopping │
         └────┬────┘     │     └────┬────┘
              │Resume    │          │StopCompleted
              └────►┌────┘          ▼
                    │          ┌─────────┐
                    │          │ Stopped │──(Start)──►Starting
                    │          └─────────┘
                    │
              Fail(msg)
                    ▼
              ┌─────────┐
              │  Error  │──(Start)──►Starting
              └─────────┘
```

`Stopped`/`Error` both accept `Start` and return to `Starting` — restarting
 the same instance record works the same way a fresh `Created` instance does
 (`Daemon::start_instance` is generic over the source state). `is_terminal()`
 returns `true` for both — that means "this run has ended", not "no
 transitions remain".

Any active state (Created/Starting/Running/Paused/Stopping) can transition to `Error` via `Fail(msg)`.

## Snapshot Mechanism

Disk-only internal qcow2 snapshots — no VM-state (RAM) serialization, so they work on any
GPU/audio/CPU configuration (the old `snapshot-save` VM-state path is blocked by QEMU's
migration machinery on every accelerated default: `virtio-sound`, `virgl`, and the
`invtsc` CPU flag are all non-migratable by design).

1. **Create** (live, instance must be Running/Paused): QMP `blockdev-snapshot-internal-sync`
   `{device, name}` — synchronous, writes a qcow2 internal snapshot of `drive-disk0`
2. **Restore** (offline, instance must NOT be Running/Paused): `qemu-img snapshot -a <tag> <disk>`
   via `andler_disk::qcow2::restore_internal_snapshot` — the guest boots from the snapshot
   state on next start; RAM is not restored (no live revert exists in QEMU)
3. **Delete** (live, instance must be Running/Paused): QMP
   `blockdev-snapshot-delete-internal-sync` `{device, name}`
4. **List**: `query-block` → extract snapshot metadata

Snapshot metadata (tag, description, created_at) stored in SQLite `snapshots` table with `ON DELETE CASCADE` from `instances`. Maximum 20 snapshots per instance (`MAX_SNAPSHOTS_PER_INSTANCE`); free space is pre-checked via `statvfs(2)` with guest RAM size as a conservative upper bound (`InsufficientDiskSpace`).

`RestoreSnapshot` on a running instance fails with `FAILED_PRECONDITION` (`InstanceMustBeStopped`) — stop the instance first, then restore, then start again.

## Metrics Collection

All host-side metrics from `/proc` — no QMP communication needed for metrics:

| Metric | Source | Calculation |
|--------|--------|-------------|
| CPU% | `/proc/<pid>/stat` (utime+stime), `/proc/uptime` | `delta(utime+stime) / delta(uptime) / num_cpus * 100` |
| RAM | `/proc/<pid>/status` (VmRSS) | Direct read |
| Disk I/O | `/proc/<pid>/io` | Delta-based bytes/sec |
| Net I/O | `/proc/<net/dev>` | Delta-based bytes/sec |
| VRAM Used | AMD: `mem_info_vram_used`, NVIDIA: NVML (`nvml-wrapper`) / `nvidia-smi` fallback, Intel: not available | Vendor-specific |
| VRAM Total | AMD: `mem_info_vram_total`, NVIDIA: NVML / `nvidia-smi` fallback, Intel: not available | Vendor-specific |
| GPU Load | AMD: `gpu_busy_percent`, NVIDIA: NVML / `nvidia-smi` fallback, Intel: `power/rc6_residency_ms` idle-time delta | Vendor-specific |

Polling interval: 1 second. Broadcast via `tokio::sync::broadcast`.

GPU vendor detection priority: AMD → NVIDIA → Intel (first found wins). AMD uses direct sysfs reads. NVIDIA uses NVML (`nvml-wrapper` crate, primary) with `nvidia-smi` CLI fallback. Intel uses `i915` sysfs `power/rc6_residency_ms` (documented idle-time ABI) for GPU load, derived from a real elapsed-time delta; Intel has no VRAM metric (stolen-memory accounting is a `debugfs`, not `sysfs`, interface).

## Future Directions
- **Bridge network mode**: Implemented in `andler-net` using `iproute2` for bridge creation and network configuration. Isolated mode is config-representable but not yet implemented (`setup_isolated` returns an explicit error)
- **GPU passthrough**: VFIO-based `RenderBackend::Passthrough`
- **Cloud Hypervisor backend**: `andler-vmm` with `rust-vmm` crates
- **GUI**: Tauri-based client (planned, not started)
- **Guest image pipelines**: Automated Android image builds with Waydroid

## gRPC Protocol

The daemon (`andlerd`) exposes a gRPC API for instance management. Protocol schema: `services/andler-rpc/proto/andler.proto`.

### Design Philosophy

The RPC layer mirrors `Daemon` trait methods — each RPC method corresponds to a real `Daemon` implementation. Adding an RPC method without a matching `Daemon` method would design the protocol "in the blind", which was explicitly avoided.

### Key RPC Methods

| RPC | Domain Method | Description |
|-----|---------------|-------------|
| CreateInstance | Daemon::create_instance | Creates LinuxVm from explicit InstanceConfig |
| CreateAndroidInstance | Daemon::create_android_instance | Resolves profile, creates overlay, registers instance |
| CloneInstance | Daemon::clone_instance | Clone AndroidVm only (requires managed instances_root) |
| ExportInstanceDisk | Daemon::export_instance_disk | Export disk without creating instance |
| StreamResourceMetrics | metrics stream | CPU, RAM, disk I/O, network I/O, VRAM, GPU load |
| StreamInstanceLogs | log stream | Streams stdout/stderr of hypervisor process |

### Type Mapping

Proto messages mirror `andler_core` domain types:

- **Enums**: CpuPriority, DiskFormat, DisplayEngine, CdromBus, InstanceState
- **oneof**: RenderBackend (carries gpu_pci_id), NetworkMode (carries interface)
- **Configuration**: Proto types correspond 1:1 to `andler-core/src/config/` types

Conversion logic: `services/andler-rpc/src/convert.rs`.

### Design Notes

- CloneInstance/ExportInstanceDisk support both AndroidVm and LinuxVm
- CreateInstance is limited to LinuxVm (field iso_path, no oneof kind) — AndroidVm path is served by separate CreateAndroidInstance to avoid two ways to create Android instances
- CreateInstance intentionally doesn't accept id/backend — daemon generates instance_id, backend is always QEMU

## Configuration Types

Configuration is organized into 9 sections in `InstanceConfig` (plus metadata fields `id`, `name`, `kind`, `backend`):

1. **disk**: Path, format (qcow2/raw/vdi), size, base image, thin provisioning, trim/compact on shutdown, snapshot timeout. The instance disk is always named `disk.qcow2` inside the instance directory. The CD-ROM is not part of `DiskConfig` — it lives in `InstanceKind::LinuxVm` as `cdrom_bus` (`VirtioScsi`/`Ide`, pre-resolved from `auto` via `recommended_for_iso_filename()`)
2. **cpu**: vCPU count (`cores`/`sockets`/`threads`), affinity (CPU pinning), priority class (`Low`/`Normal`/`High`)
3. **memory**: `size_bytes` (not MiB), ballooning, zram, ksm
4. **display**: `resolution` (width/height), dpi, fps_limit, display engine (`Sdl`/`Gtk`/`Spice`/`Dbus`/`None`), fullscreen. Clipboard lives in `input`, not here. The resolution is applied inside the guest: passed as QEMU fw_cfg (`opt/andler/display-resolution`) and read by guest units (`andler-display-resolution.service` → `/etc/andler/display.conf`); a running VM can be switched live via `set_guest_display_resolution` over the guest agent
5. **gpu**: Render backend (Venus, VirtioGpu, VirGl, Cpu, Passthrough), hostmem bytes, blob, gl
6. **network**: Mode (Nat/Bridge/Isolated), NAT backend (Slirp/Passt), interface. Isolated is accepted by config but not implemented in `andler-net`
7. **audio**: Backend (`Pipewire`/`Pulseaudio`/`None`), device (`VirtioSound`/`Ich9Hda`)
8. **input**: Pointer mode (Tablet/Mouse), hide_host_cursor, clipboard_enabled
9. **firmware**: enable_uefi, OVMF code/vars paths. The ARM translator is not part of firmware — it lives in `AndroidProfile`

Each section has reasonable defaults from hardware auto-detection (see `services/andler-firmware`). `InstanceConfig::validate()` rejects zero cores/memory/disk sizes and zero resolutions.

Disk defaults: 256 GiB thin-provisioned qcow2.

## Daemon Methods

The `Daemon` struct orchestrates all operations. Key methods:

| Method | Description |
|--------|-------------|
| `resolve_instance_id` | Resolves user-supplied instance reference to concrete `InstanceId` |
| `create_instance` | Registers new instance with `Created` state |
| `create_android_instance` | Resolves `AndroidProfile` to full instance and registers it |
| `create_linux_instance` | Creates Linux instance with disk and OVMF VARS |
| `start_instance` | Starts instance (Created/Stopped/Error → Starting → Running) |
| `stop_instance` | Stops instance (graceful/force) |
| `pause_instance` | Pauses running instance |
| `resume_instance` | Resumes paused instance |
| `remove_instance` | Removes instance record (with optional disk purge) |
| `install_guest_agent` | Installs package in guest OS |
| `remove_guest_agent` | Removes package from guest OS |
| `list_guest_packages` | Lists installed packages in guest OS |
| `switch_arm_translator` | Switches ARM translator in offline mode |
| `set_instance_config` | Partial config update |

Each method corresponds to a real `Daemon` implementation and follows the FSM transitions.