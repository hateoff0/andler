# Architecture

ANDLER (**ANDLER** = *Android Linux Emulator & Runtime*) is a Rust monorepo for running and managing virtual machines (Android and Linux guests) on Linux/KVM. It follows a daemon + thin CLI architecture, with QEMU as the current hypervisor backend and gRPC as the communication protocol.

## Design Principles

1. **Domain-driven separation**: Pure domain types in `andler-core` with no infrastructure dependencies. All I/O (QEMU, filesystem, network, database) lives in separate service/backend crates.
2. **Backend abstraction**: The `HypervisorBackend` trait defines a hypervisor-agnostic interface. Adding a new hypervisor means implementing this trait — no changes to daemon, CLI, or RPC.
3. **Explicit state machine**: Instance lifecycle is governed by a strict FSM with named states and validated transitions. No implicit state changes.
4. **Persistence is optional**: SQLite holds only snapshot metadata; the daemon works with or without a store. Instance configs live in `instance.toml` files and are re-read on every state transition — the file, not memory, is the source of truth for config. Store errors are logged but don't fail operations.
5. **No global state**: Each crate has clear responsibilities. `andler-core` knows nothing about QEMU, gRPC, or SQL. `andler-qemu` knows nothing about the daemon or CLI.
6. **User-mode by default**: All paths under `~/.andler/`. No root/sudo required for normal operation.

## Crate Dependency Graph

```
andler-core          (no workspace dependencies — bottom layer)
    ↑
    ├── andler-qemu     (depends on: core)
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
- `qmp.rs`: `QmpClient` — QMP protocol over unix socket (handshake, pause/resume/status, snapshot job API) **and** the QEMU guest agent (QGA) protocol via `connect_agent()` on the `*.qga.sock` chardev (`guest-ping`/`guest-exec`/`guest-exec-status`/`guest-file-open`/...). Async events (SHUTDOWN, DEVICE_DELETED, VSERPORT_CHANGED, BLOCK_IO_ERROR, ...) arrive on a **dedicated second QMP monitor** (`-qmp <qmp.sock>.events.sock`): `QmpEventReader` connects there exclusively and publishes events to a broadcast (it never sends commands, so event delivery cannot interleave with command/reply traffic on the command monitor — a reader task sharing the command socket wedged one gRPC connection on QEMU death, which is why the monitors are separate). `QemuBackend` spawns a reconnecting reader per instance and relays records onto its own broadcast; the daemon maps the handle back to the instance id and emits `DaemonEvent::Qmp` on the bus (`StreamEvents` RPC / `andler events`).
- `backend.rs`: `QemuBackend` — ties everything together, manages `RunningInstance` registry; guest operations (`guest_exec_package`, `set_guest_display_resolution`, ...) talk to the agent chardev socket, **not** the QMP monitor (QEMU ≥ 9 no longer registers `guest-*` commands on QMP)
- `metrics.rs`: Background poller reading `/proc/<pid>/stat`, `/proc/<pid>/status`, `/sys/block/*/stat`, `/proc/<net/dev` (per-VM); calls into `andler-firmware::metrics` for the GPU fields (host-level, not per-VM — moved there to sit next to GPU vendor detection). Sample collection runs in `tokio::task::spawn_blocking` — the `/proc` and sysfs reads are synchronous I/O and must not block the async runtime.

**Tests**: unit + integration, across `cmdline`/`process`/`qmp`/`backend`/`metrics` (GPU metrics tests moved to `services/andler-firmware`).

### `services/andler-disk` — Disk Operations

Wrapper around `qemu-img` for disk creation/cloning/resizing, plus guest tools offline provisioning.

**Key components:**
- `qcow2.rs`: 7 async functions wrapping `qemu-img` CLI
- `overlay.rs`: Android-specific overlay disk creation + factory reset
- `clone.rs`: 3 clone modes (linked, full-standalone, shared-base)
- `nbd.rs`: nbd device management with flock-based locking, `nbd_status()`, and the chroot environment setup (`bind_host_mounts`): the guest's `/etc/resolv.conf` is *written* with the host's nameservers (via the `guest-write` helper subcommand — a dangling symlink would make a bind-mount fail with ENOENT), `/dev`, `/proc`, `/sys` are bind-mounted, and a fresh tmpfs is mounted on the guest's `/run` (gpg-agent, used by pacman, needs a writable `/run`)
- `guest_offline.rs` + `guest_tools.rs`: zero-root offline guest provisioning — the disk is mounted via `guestmount` (libguestfs FUSE) and package-manager commands run chrooted inside an unprivileged user namespace (`unshare --user --map-root-user --mount`); detects the package manager (apt-get/dnf/pacman), refreshes package indexes and installs/removes packages. No `/dev/nbd*`, no root, no sudoers rules
- `arm_translator.rs`: ARM translator package staging in guest images (atomic staging + rename)
- `boot_mode.rs`: Android/Linux boot-mode switching by re-pointing the guest's `default.target` symlink through a `GuestMutator` (`switch_boot_mode_with`) — offline via the `GuestfsMutator` appliance, online via QGA; reading the mode is config-backed (P31), no disk access
- `diskspace.rs`: free-space pre-check before snapshots

**~45 unit tests** (+ ignored integration tests requiring qemu-img).

### `services/andler-guestfs` — Offline Guest Mutation

`GuestfsMutator`, one of the two real implementations of
`andler_core::GuestMutator` (the other is `QgaMutator` in
`backends/andler-qemu`). Drives the libguestfs appliance (`guestfish`)
against a guest image: the appliance boots its own unprivileged QEMU,
mounts the filesystem under an exclusive qemu image lock and applies a
batch of `MutatorOp` mutations in one session. Zero root; the package
install/remove path stays on the chroot kitchen (spike-verified
suspended variant, §6). The trait + batch contract + shared conformance
suite live in `andler-core`.

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

SQLite store for snapshot metadata only.

**Schema:**
- `snapshots(id, instance_id, tag, description, created_at, layer_path, parent_id, branch)` — chain metadata (layer path, parent, branch)
- `config_migration` marker: legacy databases (pre-phase-1, with an `instances` table) are migrated on daemon startup — each stored config is written out as `instance.toml` (refusing to overwrite a differing file), then the instances table is dropped. The daemon itself never persists instances to SQLite.

**~20 tests** using in-memory SQLite, including legacy-database migration.

### `services/andler-rpc` — gRPC Protocol

Protobuf definitions and generated code via `tonic`/`prost`.

**30 RPCs** covering instance lifecycle, monitoring, snapshots, clone/export, hotplug.
**~45 conversion tests** for bidirectional proto↔domain type mapping.

### `apps/daemon/` — Background Service

Orchestrates all operations. Holds backend registry, per-instance supervisors, optional persistence.

**Key design:**
- `Daemon::new()` / `Daemon::restore(store)` — two construction paths (restore also takes a root for tests)
- **Instance registry on disk**: each instance is a directory `~/.andler/instances/<id>/` whose `instance.toml` is the single source of truth for the config; the daemon re-reads it on every state transition (so hand-edits survive daemon restarts). `Daemon::restore()` scans the instances directory, tracks entries with missing/invalid toml as *broken* (listed with the reason, removable, never fatal to startup), migrates legacy store configs, and reconnects to any QEMU process that survived a crash (`adopt`, below). The SQLite store holds only snapshot metadata — instance configs and states never touch it.
- **Instance supervisor (task per instance)**: each registered instance runs one tokio task that owns the FSM state, the backend handle and the config — the only writer of all three. Everything else reads them through `tokio::sync::watch` snapshots (`SupervisorHandle::state()/backend_handle()/config()`) and mutates them through an mpsc command channel (`transition`, `set_handle`, `set_config`), which acknowledges only after the change is applied and persisted. This replaces the old shared `RwLock<HashMap<…>>` as the daemon's structural state. Long-running operations (snapshot restore is the first) run as supervisor sub-tasks that ack their start immediately through a oneshot and stream progress back (`RunOperation`/`CancelOperation`/`GetActiveOp` in `daemon/ops.rs`) — one operation per instance, cancellation via a watch token checked at per-file phase boundaries; `Operation` events land on the event bus and in the instance audit log. QMP/metrics ownership moves under the supervisor in a later phase.
- **Daemon restart semantics**: a graceful SIGTERM/Ctrl+C is a deliberate shutdown — `shutdown_signal` stops every Running/Paused instance before the daemon exits and `kill_on_drop` guards the rest. Reconnect is therefore a crash-recovery path, not a graceful-restart path: on startup `Daemon::restore()` tries to adopt any instance whose QEMU process is still alive by connecting to its QMP socket, resolving the surviving QEMU's pid (`query-processes`, falling back to the `/proc` `process=<name>` cmdline marker), verifying identity, and re-creating the backend handle around the pidfd. Only if the process cannot be found/recovered does the instance land in `Stopped`. Instances that were mid-operation (Starting/Stopping) at crash time are never guessed at: they become `Stopped`, and `start` is the documented recovery path.
- **Event bus**: the daemon owns a `broadcast::Sender<DaemonEvent>`; supervisors publish `Lifecycle` events on every applied FSM transition (with the `Fail` reason). Consumers subscribe via `Daemon::subscribe_events()` (event types live in `andler-core::events`, pure serde types).
- Instance lifecycle via FSM transitions (applied by the supervisor)
- `InstanceDirGuard` RAII for cleanup on partial failure
- `create_linux_instance()` / `create_android_instance()` — high-level resource creation + registration
- `resolve_instance_id()` — Docker-style partial ID resolution (12-char hex prefix)
- `update_instance_config()` — Edit config via gRPC, protects id/kind/disk.path
- `DaemonService` — thin gRPC wrapper, one method per Daemon method
- Error mapping: `DaemonError` (count from `error.rs`) → `ErrorKind` (single exhaustive match) → gRPC status codes

**~120 unit tests** (some ignored) + **gRPC round-trip tests** (real TCP).

### `apps/cli/` — Command-Line Interface

Thin gRPC client. Each subcommand = one gRPC request + print response.

**Key features:**
- Unified `create` command (TOML for LinuxVm, CLI flags for AndroidVm)
- Interactive wizard with smart defaults and hardware auto-detection
- Real-time metrics streaming with GPU columns (AMD/NVIDIA/Intel)
- Snapshot CRUD with per-operation timeout
- Disk management (create, info, resize with shrink protection, compact)
- Shell completions (bash, zsh, fish)
- Colored status output with `IsTerminal` gating
- `doctor` command for environment diagnostics and auto-fix (KVM/QEMU/OVMF/nbd/sudoers/daemon/base images + `CAP_NET_ADMIN` for bridge networking)
- `op list`/`op cancel` — long-operation progress and cancellation
- `connect` (console/ssh/adb/exec) — guest access levels: console attaches straight to the serial chardev socket (raw termios via libc, piped-stdin safe); ssh/adb spawn the external client against `network.port_forwards`; `exec` runs through the guest agent and relays the exit code. `auto` picks by the effective `(kind, boot_mode)` profile
- `create --template` — VM templates (built-ins + `~/.andler/templates/`), merged defaults < template < flags
- Version handshake: `GetVersion` is queried before every daemon-bound command; a mismatched daemon build is rejected with a restart hint
**~130 tests** (TOML parsing, helpers, create, wizard, status, doctor, connect).

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
  ↓ (SQLite, snapshot metadata only)
andler-store (andlerd.db)
```

## Guest Provisioning (`guest` operations)

Package management and resolution changes inside a guest use a two-tier strategy:

**Online (VM `Running`, guest agent available)**: commands run inside the guest via the QEMU guest agent (QGA) — `guest-exec`/`guest-exec-status` (package install/remove, package-manager detection) and `guest-file-*` (config file writes, e.g. `display.conf` on resolution change). The agent wire runs over the dedicated `*.qga.sock` chardev (`virtserialport name=org.qemu.guest_agent.0`), never the QMP monitor. This requires `qemu-guest-agent` inside the guest, which the base image ships enabled (`qemu-guest-agent.service`).

**Offline path (VM stopped)** — the disk is mounted via `guestmount` (libguestfs FUSE, zero root) and operations run chrooted inside an unprivileged user namespace (`unshare --user --map-root-user --mount`):

- package manager detected by binary (`apt-get`/`dnf`/`pacman`); package index is refreshed first (`apt-get update` / `dnf makecache` / `pacman -Sy`) so installs work on fresh images
- the chroot is made network- and signature-capable by `bind_host_mounts`: host nameservers are written into the guest's `/etc/resolv.conf` (via `guest-write`, replacing the dangling `stub-resolv.conf` symlink), `/dev`, `/proc`, `/sys` are bind-mounted, and a tmpfs is mounted on the guest's `/run` (gpg-agent needs it for pacman signatures)
- nothing is privileged: `guestmount` needs only `/dev/fuse` access, `unshare --user` needs unprivileged user namespaces allowed by the kernel (Debian/Ubuntu: `sysctl kernel.unprivileged_userns_clone=1`); the old qemu-nbd + `andler-helper` sudoers surface is gone entirely

ARM translators (`libndk`/`libhoudini`) use the same offline mount machinery via `switch_arm_translator`: version-keyed download (MD5-verified, cached under `~/.andler/cache/arm-translators/`), staged into `var/lib/waydroid/overlay/system`, `build.prop` updated, old translator removed only after the new one is fully staged.

## Data Flow (guest operations)

```
andler CLI → gRPC → daemon
  ├─ running: backend.qga socket (*.qga.sock) → qemu-ga (guest) → guest-exec/file ops
  └─ stopped: andler-disk → guestmount (FUSE) → unshare --user --map-root-user --mount → chroot → package manager (apt needs APT::Sandbox::User=root + ForceIPv4 in userns)
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

## Guest Readiness Contract

The daemon's `status` reports the guest's readiness as an explicit level, not a
guess from log text. The contract (decision from the architecture rework,
documented here) is a monotonic ladder — each level implies the previous one:

| Level | Meaning |
|---|---|
| `SerialUp` | VM process is up, serial chardev responds |
| `QgaUp` | QEMU guest agent answers probes (QGA chardev socket) |
| `DisplayApplied` | configured display resolution was applied inside the guest |
| `GuestOsUp` | guest OS finished booting (systemd / init reports ready) |
| `WaydroidReady` | Android session: Waydroid container is running (AndroidVm only) |

**Effective profile, not just `kind`**: the terminal level depends on the
`(kind, boot_mode)` pair, because an `AndroidVm` switched to Linux boot-mode
never brings up Waydroid. For `LinuxVm` (and Android-in-Linux-mode) the
terminal level is `GuestOsUp`; for Android-in-waydroid-mode it is
`WaydroidReady`. Callers that consume readiness (status display, guest access
levels) must derive the terminal level from the effective profile, never from
`kind` alone.

How levels are reached is the guest's own reporting (systemd units + QGA
probes + fw_cfg phase marker); the readiness event flows through the
daemon event bus as `DaemonEvent::Readiness { level }` (see `andler-core`
events). The implementation of the reporting mechanism itself is phase 3
work (QMP/QGA subscription); the contract above is the stable interface.

## Snapshot Mechanism

Disk-only **external** qcow2 snapshots — no VM-state (RAM) serialization, so they work on
any GPU/audio/CPU configuration (the old `snapshot-save` VM-state path is blocked by QEMU's
migration machinery on every accelerated default: `virtio-sound`, `virgl`, and the `invtsc`
CPU flag are all non-migratable by design). Every snapshot of a qcow2 disk creates an
overlay **layer file**; the disk the VM actually uses is always `disk.qcow2`, and layers
live in `<instance-dir>/disk.snapshots/<uuid>.qcow2` (staging files are
`disk.snapshots/.tmp-<uuid>.qcow2` and are never scanned as layers).

1. **Create** (live, instance must be Running/Paused): the primary disk must be qcow2
   (`SnapshotRequiresQcow2` otherwise — convert first). A staging overlay is created
   (`qemu-img create`), its backing reference is pointed at the *future* layer path with
   `qemu-img rebase -u -F qcow2` (the file does not exist yet), then QMP
   `blockdev-add` (overlay node with `backing:null`, `file.locking:off`) +
   `blockdev-snapshot {node: drive-disk0, overlay: snap-<uuid>}` switches the live disk
   graph. Finally a rename pair moves the old active file into
   `disk.snapshots/<uuid>.qcow2` and the overlay into place as the new `disk.qcow2`.
   Metadata is persisted *last*; a crash anywhere before that leaves a recoverable state
   (see Reconciliation below).
2. **Restore** (offline, instance must NOT be Running/Paused):
   - **Default (discard)**: the target must be on the main branch
     (`RestoreTargetOnArchivedBranch` otherwise — pass `--branch` to switch to it).
     Every layer newer than the target on the main branch is deleted (file + metadata),
     the active disk is removed, and a fresh `disk.qcow2` overlay is created on top of the
     target — the linear history continues from the target.
   - **`--branch`**: nothing is deleted. The current active chain is archived as a branch:
     the active disk becomes a layer tagged `pre-branch-<ts>`, and every main-branch
     layer record is marked with `branch-<ts>`. A new `disk.qcow2` overlay is created on
     top of the target. If the target lives on an archived branch, that branch's ancestors
     are moved back to the main branch (its layers become restorable again) and the new
     active disk continues from the target. Branches are read-only in this phase: layers
     on archived branches can be listed and deleted, but discard-restore refuses them.
3. **Delete** (offline, instance must NOT be Running/Paused): deleting a layer commits its
   data into its parent (`qemu-img commit`), re-points its direct children at the parent
   (`qemu-img rebase -u`, including the active disk when it backs onto the layer), then
   removes the layer file and its metadata entry. The base layer (no parent) cannot be
   deleted (`CannotDeleteBaseLayer`). Legacy internal snapshots (created before this
   phase) are still deleted with `qemu-img snapshot -d`; they are read-only for restore
   (`SnapshotInternalNotRestorable`).
4. **List**: metadata rows (all branches) merged with internal snapshots still reported by
   QEMU `query-block`; each entry carries its `branch` (empty = main branch).
5. **Clone protection**: linked clones derive their disk from the source's active file,
   so their backing chain reaches the source's layers. Restore is refused while live
   clones exist (`RestoreWouldBreakClones` — the active disk is rebuilt, orphaning them),
   and deleting a layer is refused while another instance's chain contains it
   (`DeleteWouldBreakClones`, discovered by walking qcow2 backing files).

Snapshot metadata (id, tag, description, created_at, layer path, parent layer id, branch)
is stored in SQLite `snapshots` (schema v3; legacy v0–v2 tables migrate in place). Maximum
20 snapshots per instance (`MAX_SNAPSHOTS_PER_INSTANCE`); free space is pre-checked via
`statvfs(2)` with guest RAM size as a conservative upper bound (`InsufficientDiskSpace`).

`RestoreSnapshot`/`DeleteSnapshot` on a running instance fails with
`FAILED_PRECONDITION` (`InstanceMustBeStopped`) — stop the instance first, then restore or
delete, then start again.

### Chain Reconciliation

On every daemon startup the daemon reconciles each qcow2 instance's on-disk chain against
the store (`disk_chain::reconcile_chain`). With a live adopted VM only metadata is touched;
otherwise the disk is repaired too:

- `disk.qcow2` missing + an orphaned `.tmp-*` overlay present → the overlay becomes the
  active disk (crash between the rename pair; the chain is fully recoverable).
- `disk.qcow2` missing + layers present → a fresh active disk is rebuilt on top of the
  newest layer (crash mid-restore).
- Layer file without a metadata entry → entry is inserted as `recovered-<uuid8>`.
- Metadata entry whose layer file vanished → entry is deleted (tampering/accidental
  removal is not silently re-created).
- Stray `.tmp-*` files are removed. A chain that cannot be inspected is left alone with a
  warning; orphaned layer files not referenced by any record are also left alone.

**Known limitation (serialization)**: snapshot operations are only serialized by the state
machine — `create` requires Running/Paused, `restore`/`delete` require Stopped — so
`create` never races `restore`/`delete`. Two simultaneous `restore` (or `delete`) calls on
a stopped instance are not yet guarded by a per-instance op-mutex; the single-writer
discipline for chains lands with the DiskOps phase (see `docs/ROADMAP.md`). Until then the
daemon's RPC handler processes restore/delete synchronously, which makes the window
negligible but not formally excluded.

## Hotplug Mechanism

Extra disks and network devices can be attached to a `Running`/`Paused` instance and detached again, and are re-created automatically at the next boot.

1. **Persistence**: attached devices live in `InstanceConfig.extra_disks` / `extra_networks` (serde-defaulted `Vec`s, so old `instance.toml` files load unchanged). The daemon appends to the list *after* the backend confirms the live device, and persists via the same `instance.toml` rewrite path as any config change.
2. **Identity**: devices are addressed by list index; the backend derives QEMU ids from it (`drive-extraN`/`extraN` blockdev/device, `net-extraN` netdev/device, bridge taps `tap<instance-id>-eN` so multiple VMs on one bridge never collide). Detach identifies disks by **path** and networks by **index**, so cmdline wiring at boot and QMP hot-plug always agree.
3. **Live attach** (QMP): `blockdev-add` (`{"driver":"qcow2","node-name":"drive-extraN",...}`) + `device_add` (virtio-blk-pci), or `netdev_add` (user/tap/passt) + `device_add` (net model). On failure the backend rolls back: `blockdev-del` after a failed `device_add`, host tap teardown after a failed netdev setup.
4. **Live detach** (QMP): `device_del` is asynchronous — QEMU completes it on its own schedule, so the follow-up `blockdev-del`/`netdev-del` may hit `DeviceInUse`. The daemon retries only while QEMU reports the device in use (`DeviceInUse` class or `in use` description, 250ms × 15s); any other `CommandFailed` fails immediately.
5. **Boot re-attach**: the cmdline builder emits the extra `-drive`/`-device`/`-netdev` args in list order; host taps for bridge mode are created before spawn and torn down on stop.
6. **Limitations**: snapshots (`snapshot create`) cover the primary `drive-disk0` only — extra disks are not snapshotted. Attached disks are identified by absolute path; a relative or bare-name path resolves into the instance directory at attach time.

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

## Log Rotation & Retention

Instance logs must not grow without bound (a long-running guest filled 4.4 GB
of disk with logs). Policy (decision from the architecture rework):

- `qemu.log` and `console.log` in the instance directory rotate logrotate-style
  by **size**, enforced by the daemon (not by the guest): when a file exceeds
  a size cap (default 16 MiB), it is renamed to `.<N>` generations (default 3
  kept, newest-highest), rotation happens at a safe point (daemon-driven, not
  mid-write). Rotated generations are capped by the same size rule.
- The daemon's own log directory (`~/.andler/logs`, when used) applies
  **retention by age + total size**: generations older than 30 days or beyond
  a total budget (default 256 MiB) are pruned.
- Rotation is a daemon mechanism, not a CLI concern: `andler logs` reads
  through the same file, so a rotated file never breaks the stream contract.

### Offline zero-root: FUSE mount flags

The offline package path (`--offline`) mounts the guest disk via
`guestmount` (FUSE) and runs the package manager chrooted in a user
namespace. Three mount options make this work for a non-root user:

- `-o uid=<euid> -o gid=<egid>` map every guest uid/gid to the mounting
  user, so inside the namespace the guest files belong to root. Two
  separate flags: the comma form `-o uid=X,gid=Y` silently drops gid in
  libguestfs 1.48.
- `-o default_permissions` enables real POSIX permission checks. Without
  it the kernel answers `access(2)` from its stricter FUSE path that
  compares the caller against the mount owner and rejects non-root even
  when the files belong to the caller; dpkg aborts with "required
  read/write access to the dpkg database directory". With the flag,
  access is decided by mode bits against the mapped owners, which is
  exactly what dpkg's `access(R_OK|W_OK)` needs. Verified with a full
  `dpkg -i` inside the chroot on the cloud image.

The daemon still probes `test -w /var/lib/dpkg` in the chroot right
after mounting and fails with the workaround named instead of failing
after a minutes-long apt run, in case a host combination breaks one of
these assumptions in the future. `subuid`/`newuidmap` range mapping is
not needed: the single identity required inside the chroot (guest root)
is covered by the uid/gid mapping alone. The smart online path is
unaffected.

### Log redaction (§9.1.5)

The daemon log (and therefore the `andler logs daemon` ring, which carries
the same bytes) never contains guest-controlled or guest-sourced content at
the default INFO level:

- **guest-exec stdout/stderr is never logged** (package install/remove,
  `andler exec`, mutator commands) — it can carry passwords or tokens the
  guest printed; failures surface through returned errors instead.
- **QEMU/guest output lines are debug-only** — the guest can print anything
  into the console, so qemu.log + `andler logs <id>` are the guest-log
  stream, never the daemon log (§9.1.6).
- **Provision manifests log op counts only**, never file contents or host
  paths of uploads; guest file reads for diagnostics (resolv.conf) are
  debug-only.
- Env values are never logged; instance ids, paths, and error text (the
  actionable, sanitized message, not raw stderr) are the allowed fields.

### Request correlation (§9.1.1)

Every CLI request carries a `request_id` metadata header (32-hex, generated
per request); the daemon's RPC layer wraps each handler in an
`rpc{request_id=…, method=…}` span, so any event emitted while handling a
request shows up in the daemon log with its id. Long operations keep their
own `op_id` on the event bus; together they reconstruct
CLI → RPC → operation from one log line. Retry loops (QMP events monitor,
health checks) log coalesced — first failure, every 50th attempt, and a
recovery summary (§9.1.4).

## Security / Threat Model

Honest model, written down because the helper doc referenced it (see ROADMAP)
and it was absent. Scope: single-user Linux desktop running QEMU/KVM.

- **Boundary**: the VM is the isolation boundary — guests are untrusted. The
  daemon runs as the user, not root; after the guestfs migration (see ROADMAP)
  the daemon performs no privileged operations at all. The
  `andler-helper` entry point narrows and structure-hardens the remaining
  privileged surface (argument injection, path traversal, symlink confusion),
  but a compromised daemon remains root — it is **not** a sandbox; the
  boundary is against bugs and other users.
- **IPC boundary**: `andlerd` listens on TCP loopback (`ANDLERD_LISTEN_ADDR`,
  default `127.0.0.1:50051`). Loopback bounds the network, not the user; the
  rework explicitly chose the documented assumption **one host user per
  machine** (multi-user trust is out of scope; a unix-socket transport with
  `0600` is the documented upgrade path if that assumption ever changes).
- **Untrusted artifacts**: snapshots and base-image manifests may be
  corrupted or malicious (a broken snapshot must not wedge the daemon):
  manifests are validated (shape, required fields, size limits) before use;
  snapshot restore validates the image before switching the disk; resource
  limits (time/size) apply to guest-driven operations.
- **Per-instance files**: instance directory, QMP/QGA sockets, and SSH key
  material use the same private-permissions discipline as the rest of
  `~/.andler` (0700/0600); QMP/QGA sockets live under
  `$XDG_RUNTIME_DIR/andler/` which is user-private by construction.

## Future Directions
- **Bridge network mode**: Implemented in `andler-net` using `iproute2` for bridge creation and network configuration. Isolated mode is config-representable but not yet implemented (`setup_isolated` returns an explicit error)
- **GPU passthrough**: VFIO-based `RenderBackend::Passthrough`
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

Configuration is organized into 9 sections in `InstanceConfig` (plus metadata fields `id`, `name`, `kind`, `backend`), two hotplug lists (`extra_disks`, `extra_networks`), and the primary config sections:

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
| `attach_disk` / `detach_disk` | Hot-plug/unplug an extra disk (Running/Paused only) |
| `attach_network` / `detach_network` | Hot-plug/unplug an extra network device (Running/Paused only) |

Each method corresponds to a real `Daemon` implementation and follows the FSM transitions.