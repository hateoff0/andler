# API Reference

## CLI Commands

All commands connect to the daemon via gRPC. Pass `--daemon-addr <url>` before the subcommand to target a non-default daemon.

### `create`

Creates a new instance. Three modes:

**TOML mode** (`--file`): reads instance config from a TOML file. Creates a `LinuxVm` or `AndroidVm`, auto-detected by content (presence of `android_version` or `base_image_path` → AndroidVm).

```bash
# LinuxVm from TOML
andler create --file instance.toml

# AndroidVm from TOML
andler create --file android.toml
```

**CLI mode** (`--kind`): creates an instance via flags. `--kind` selects the VM type.

```bash
# LinuxVm via CLI
andler create \
  --kind linux \
  --name my-linux \
  --iso-path /path/to/installer.iso \
  --disk-path /path/to/disk.qcow2 \
  --ovmf-vars-template /path/to/VARS.fd \
  [--disk-size-gib 100] [--quick] [--cdrom-bus auto] [--compact-on-shutdown]

# AndroidVm via CLI
andler create \
  --kind android \
  --name my-android \
  --android-version 13 \
  --base-image-path /path/to/base.qcow2 \
  --ovmf-vars-template /path/to/VARS.fd \
  [--root magisk --magisk-dir /path/to/magisk/] \
  [--gapps] [--microg] [--arm-translator libndk] \
  [--overlay-size-gib 20] \
  [--instances-root /path/to/instances/]
```

**Flags:**

| Flag | Required | Description |
|------|----------|-------------|
| `--file <path>` | Yes* | Path to TOML config file (*mutually exclusive with `--kind`) |
| `--kind <type>` | Yes* | VM type: `linux` or `android` (*mutually exclusive with `--file`) |
| `--quick` | No | Skip interactive wizard, create with all defaults. Requires `--kind`. Mutually exclusive with `--file`. |
| `--name <name>` | Yes** | Instance name (**required in CLI mode) |
| `--ovmf-vars-template <path>` | Yes** | Path to OVMF_VARS template (**required in CLI mode) |
| `--iso-path <path>` | Yes*** | Path to installer ISO (***required for `--kind linux`) |
| `--disk-path <path>` | Yes*** | Path to disk file (***required for `--kind linux`) |
| `--disk-size-gib <size>` | No | Disk size in GiB (default: 256, Linux only) |
| `--cdrom-bus <bus>` | No | CD-ROM bus: `auto` (default), `virtio`, `ide` (Linux only) |
| `--compact-on-shutdown` | No | Auto-compact disk after shutdown (Linux only) |
| `--android-version <ver>` | Yes**** | Android version: `11` or `13` (****required for `--kind android`) |
| `--base-image-path <path>` | Yes**** | Path to Android base image qcow2 (****required for `--kind android`) |
| `--gapps` | No | Include Google Apps |
| `--microg` | No | Include microG |
| `--arm-translator <mode>` | No | ARM→x86 translation: `none` (default), `libndk`, `libhoudini` |
| `--root <mode>` | No | Root mode: `none` (default), `magisk` |
| `--magisk-dir <path>` | No | Path to Magisk binaries (required when `--root magisk`) |
| `--overlay-size-gib <size>` | No | Overlay disk size in GiB (default: 20, Android only) |
| `--instances-root <path>` | No | Instance directory root (default: `~/.andler/instances`) |

### `start`

```bash
andler start <instance-id>
```

Transitions: Created → Starting → Running. Fails if instance is already running or in a non-terminal error state.

### `stop`

```bash
andler stop <instance-id> [--graceful]
```

Without `--graceful`: kills the QEMU process immediately (SIGKILL) — this is the default.
With `--graceful`: sends SIGTERM and waits up to 30 seconds for graceful shutdown.

### `pause`

```bash
andler pause <instance-id>
```

Pauses a running instance via QMP `stop` command. Instance must be in `Running` state.

### `resume`

```bash
andler resume <instance-id>
```

Resumes a paused instance via QMP `cont` command. Instance must be in `Paused` state.

### `status`

```bash
andler status <instance-id>
```

Prints current state: `Created`, `Starting`, `Running`, `Paused`, `Stopping`, `Stopped`, or `Error`.

### `list`

```bash
andler list [--state <STATE>] [--name <NAME>] [--sort <FIELD>] [--json] [--full-id]
```

Lists all registered instances. Supports filtering by state and name (partial, case-insensitive).

| Flag | Description |
|------|-------------|
| `--state <STATE>` | Filter by state: `Created`, `Starting`, `Running`, `Paused`, `Stopping`, `Stopped`, `Error` |
| `--name <NAME>` | Filter by instance name (partial match, case-insensitive) |
| `--sort <FIELD>` | Sort by: `name` (default), `state`, `created` |
| `--json` | Output as JSON array |
| `--full-id` / `-q` | Show full UUID instead of 8-char short ID |

### `config`

```bash
andler config <instance-id>
```

Prints full instance configuration (all 9 sections + metadata). Output format is human-readable but not valid TOML.

### `remove`

```bash
andler remove <instance-id> [--purge]
```

Without `--purge`: removes the instance record only. Files remain on disk.
With `--purge`: also deletes `disk.path` and `firmware.ovmf_vars_path`. Never deletes `base_image` or `ovmf_code_path` (shared across instances).

Instance must be in a terminal state (`Created`, `Stopped`, or `Error`). Use `stop` first for running instances.

With `--purge`, refuses if the instance has live `Linked` clones (deleting the source disk would break them).

### `clone`

```bash
andler clone <source-id> --name <new-name> --mode <linked|full-standalone|shared-base>
```

| Mode | Description | Cost | Independence |
|------|-------------|------|--------------|
| `linked` | Overlay with `backing_file` on source disk | Cheap (no data copy) | Dependent on source |
| `full-standalone` | Flattens entire backing chain | Expensive (full copy) | Fully independent |
| `shared-base` | Byte-copy of source disk file | Moderate | Independent from source, thin relative to base |

Source must be in a terminal state (`Created`, `Stopped`, or `Error`). `LinuxVm` supports `linked` and `full-standalone`. `SharedBase` is rejected for `LinuxVm` (`SharedBaseNotSupportedForLinuxVm`).

Cloning a clone is allowed.

### `export`

```bash
andler export <source-id> <dest-path>
```

Exports the instance disk as a standalone file at `dest_path`. Does not create a new instance. Source must be in a terminal state (`Created`, `Stopped`, or `Error`).

### `logs`

```bash
andler logs <instance-id> [--source <SOURCE>] [--grep <PATTERN>] [--tail <LINES>]
```

Streams QEMU stdout/stderr with `[stdout]`/`[stderr]` prefix. Also reads historical log file content before streaming live output.

| Flag | Description |
|------|-------------|
| `--source <SOURCE>` | Filter by source: `stdout`, `stderr`, or `all` (default) |
| `--grep <PATTERN>` | Filter lines by regex pattern |
| `--tail <LINES>` | Show only the last N lines of historical output before streaming live |

If instance has no running backend (never started, or already stopped), prints a warning to stderr and exits with code 0.

### `metrics`

```bash
andler metrics <instance-id> [--once] [--json]
```

Streams resource metrics every second in key=value format:

```
cpu=12.34% rss=1.23 GiB disk_r=45.6 MB/s disk_w=12.3 MB/s net_rx=1.2 MB/s net_tx=0.5 MB/s vram=512 MiB gpu=67.8%
```

| Flag | Description |
|------|-------------|
| `--once` | Print a single snapshot and exit |
| `--json` | Output as JSON object |

GPU fields (vram, gpu) appear when AMD, NVIDIA, or Intel GPU data is available. CPU% is delta-based (delta(utime+stime) / delta(uptime) / num_cpus * 100).

### `snapshot`

```bash
# Create (requires Running/Paused instance)
andler snapshot <instance-id> create --tag before-update --description "Pre-upgrade state" --timeout 120

# Restore (requires Running/Paused instance)
andler snapshot <instance-id> restore --tag before-update --timeout 10

# Delete (requires Running/Paused instance)
andler snapshot <instance-id> delete --tag before-update --timeout 5

# List (any state)
andler snapshot <instance-id> list
```

The `--timeout` flag overrides the per-instance `snapshot_timeout_secs` for a single operation. If not specified, uses the instance default (30s).

### `disk`

Disk management commands — wraps `qemu-img` operations.

```bash
# Create a new empty disk
andler disk create /path/to/disk.qcow2 --size 64GB
andler disk create /path/to/disk.qcow2 --size 128000MB
andler disk create /path/to/disk.qcow2 --size 1T

# Show disk information
andler disk info /path/to/disk.qcow2

# Resize an existing disk
andler disk resize /path/to/disk.qcow2 --size 80GB
andler disk resize /path/to/disk.qcow2 --size 50GB --shrink  # shrink requires explicit flag

# Compact a disk (reclaim unused space)
andler disk compact /path/to/disk.qcow2
```

**Size format**: Supports `GB`, `GiB`, `MB`, `MiB`, `TB`, `TiB` (case-insensitive). Space between number and unit is optional. Plain number = bytes.

| Command | Description |
|---------|-------------|
| `create <path> --size <size>` | Create a new empty qcow2 disk (default 256 GiB) |
| `info <path>` | Show disk info (virtual size, actual usage, format, backing file) |
| `resize <path> --size <size>` | Resize an existing disk (requires `--shrink` to reduce size) |
| `compact <path>` | Compact a disk (reclaim unused space via `qemu-img convert`, only works on qcow2) |

### `guest`

Guest package management — install, remove, or list packages in the guest OS. Auto-fallback: if VM is running and guest agent is available → online via QMP `guest-exec`; if VM is stopped → offline via `qemu-nbd` + mount.

```bash
# Install a package
andler guest install spice-vdagent <instance-id>

# Remove a package
andler guest remove spice-vdagent <instance-id>

# List known packages and their status
andler guest list <instance-id>
```

Known packages: `spice-vdagent` (shared folders), `qemu-guest-agent` (host-guest communication), `spice-webdavd` (webdav shared folders).

| Command | Description |
|---------|-------------|
| `install <package> <instance-id>` | Install a package in the guest OS |
| `remove <package> <instance-id>` | Remove a package from the guest OS |
| `list <instance-id>` | List known packages and their status (installed/not installed) |

### `edit`

```bash
andler edit <instance-id> [--name <NAME>] [--disk-size-gib <SIZE>]
```

Edits instance configuration. Changes are applied to both the SQLite store and the `instance.toml` file.

| Flag | Description |
|------|-------------|
| `--name <NAME>` | New instance name |
| `--disk-size-gib <SIZE>` | New disk size in GiB |

Protected fields (`id`, `kind`, `disk.path`) cannot be changed.

### `wizard`

```bash
andler wizard
```

Interactive guided instance creation wizard. Walks through all configuration options with smart defaults and hardware auto-detection. Outputs a TOML config file for review before creation.

### `completions`

```bash
andler completions <SHELL>
```

Generate shell completions for the specified shell: `bash`, `zsh`, or `fish`.

```bash
# bash
andler completions bash > ~/.local/share/bash-completion/completions/andler

# zsh
andler completions zsh > ~/.zsh/completions/_andler

# fish
andler completions fish > ~/.config/fish/completions/andler.fish
```

## Instance TOML File

### Linux VM (minimal)

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.andler/my-linux-vm/VARS.fd"
```

### Android VM (minimal)

```toml
name = "my-android"
android_version = 13
base_image_path = "/path/to/base.qcow2"
ovmf_vars_path = "/path/to/VARS.fd"
```

**Auto-detection**: if `android_version` or `base_image_path` is present, the TOML file is treated as an AndroidVm config. Otherwise, it's a LinuxVm.

### Android VM (full example)

```toml
name = "my-android"
android_version = 13
base_image_path = "/path/to/base.qcow2"
ovmf_vars_path = "/path/to/VARS.fd"

# Optional
overlay_size_gib = 20
root = "magisk"
magisk_dir = "/path/to/magisk/"
gapps = false
microg = false
libndk = false
instances_root = "/home/user/.andler/instances"
```

### Linux VM (full example)

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.andler/my-linux-vm/VARS.fd"

disk_size_gib = 100
snapshot_timeout_secs = 60

[cpu]
cores = 8
sockets = 1
threads = 2
affinity = [0, 2, 4, 6]
priority = "High"

[memory]
size_bytes = 8589934592
ballooning = false
zram = false
ksm = true

[display]
resolution = { width = 1920, height = 1080 }
dpi = 96
fps_limit = 0
display_engine = "Sdl"
fullscreen = false

[gpu]
render_backend = "Venus"
hostmem_bytes = 4294967296
blob = true
gl = true

[network]
mode = "Nat"
device_model = "virtio-net-pci"

[firmware]
ovmf_code_path = "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd"
ovmf_vars_path = "/path/to/VARS.fd"

[audio]
backend = "Pipewire"
device = "VirtioSound"

[input]
pointer_mode = "Tablet"
hide_host_cursor = true
clipboard_enabled = true
```

### Headless Mode

```toml
[gpu]
render_backend = "Cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
display_engine = "None"
fullscreen = false

[audio]
backend = "None"
```

### Section Defaults

Any section can be omitted entirely. If present, it must be complete (no partial overrides).

| Section | Default |
|---------|---------|
| `cpu` | 4 cores, 1 socket, 1 thread, no affinity, Normal priority |
| `memory` | 8 GiB, no ballooning, no zram, KSM on |
| `disk` | 256 GiB qcow2, no backing, thin provisioning, discard |
| `display` | 1920x1080, 96 DPI, no limit, SDL, no fullscreen |
| `gpu` | Venus, 4096 MiB hostmem, blob+gl on |
| `network` | NAT, virtio-net-pci |
| `firmware` | OVMF_CODE at `/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd` |
| `audio` | PipeWire, VirtioSound |
| `input` | Tablet pointer, hide_cursor+clipboard true |

## gRPC Protocol

Defined in `services/andler-rpc/proto/andler.proto`. Uses `tonic`/`prost` for Rust code generation.

### Service: `AndlerService`

| RPC | Request | Response | Streaming |
|-----|---------|----------|-----------|
| `CreateInstance` | `CreateInstanceRequest` | `CreateInstanceResponse` | Unary |
| `CreateAndroidInstance` | `CreateAndroidInstanceRequest` | `CreateInstanceResponse` | Unary |
| `StartInstance` | `InstanceIdRequest` | `Empty` | Unary |
| `StopInstance` | `StopInstanceRequest` | `Empty` | Unary |
| `PauseInstance` | `InstanceIdRequest` | `Empty` | Unary |
| `ResumeInstance` | `InstanceIdRequest` | `Empty` | Unary |
| `GetInstanceStatus` | `InstanceIdRequest` | `InstanceStatusResponse` | Unary |
| `ListInstances` | `ListInstancesRequest` | `ListInstancesResponse` | Unary |
| `RemoveInstance` | `RemoveInstanceRequest` | `Empty` | Unary |
| `GetInstanceConfig` | `InstanceIdRequest` | `GetInstanceConfigResponse` | Unary |
| `UpdateInstanceConfig` | `UpdateInstanceConfigRequest` | `Empty` | Unary |
| `StreamInstanceLogs` | `InstanceIdRequest` | `stream LogLineResponse` | Server-streaming |
| `StreamResourceMetrics` | `InstanceIdRequest` | `stream ResourceMetricsResponse` | Server-streaming |
| `CloneInstance` | `CloneInstanceRequest` | `CreateInstanceResponse` | Unary |
| `ExportInstanceDisk` | `ExportInstanceDiskRequest` | `ExportInstanceDiskResponse` | Unary |
| `CreateSnapshot` | `CreateSnapshotRequest` | `CreateSnapshotResponse` | Unary |
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary |
| `ListSnapshots` | `ListSnapshotsRequest` | `ListSnapshotsResponse` | Unary |
| `InstallGuestAgent` | `InstallGuestAgentRequest` | `Empty` | Unary |
| `RemoveGuestAgent` | `RemoveGuestAgentRequest` | `Empty` | Unary |
| `ListGuestPackages` | `InstanceIdRequest` | `ListGuestPackagesResponse` | Unary |

### Error Codes

| gRPC Status | Daemon Error | When |
|-------------|--------------|------|
| `NOT_FOUND` | `InstanceNotFound`, `SnapshotNotFound`, `InstanceRefNotFound`, `AgentNotInstalled`, `PackageManagerNotFound` | Unknown instance/snapshot/ref/package |
| `UNIMPLEMENTED` | `NoBackendRegistered` | Backend kind not available |
| `FAILED_PRECONDITION` | `InvalidTransition`, `InstanceNotRemovable`, `InstanceNotClonable`, `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`, `SnapshotOperationRequiresRunningInstance`, `SnapshotLimitExceeded`, `GuestAgentUnavailable` | Wrong lifecycle state, resource limit, or guest agent unavailable |
| `ALREADY_EXISTS` | `SnapshotAlreadyExists`, `AgentAlreadyInstalled` | Duplicate snapshot tag or package already installed |
| `INVALID_ARGUMENT` | `ConvertError`, `EmptyInstanceRef`, `MalformedInstanceRef`, `AmbiguousInstanceId`, `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged` | Malformed request or invalid arguments |
| `INTERNAL` | Other errors | Backend/disk/store failures |

### Key Messages

**`ResourceMetricsResponse`** — All fields optional:

```protobuf
message ResourceMetricsResponse {
  optional float cpu_percent = 1;
  optional uint64 memory_used_bytes = 2;
  optional uint64 disk_read_bytes_per_sec = 3;
  optional uint64 disk_write_bytes_per_sec = 4;
  optional uint64 net_rx_bytes_per_sec = 5;
  optional uint64 net_tx_bytes_per_sec = 6;
  optional uint64 vram_used_bytes = 7;
  optional uint64 vram_total_bytes = 8;
  optional float gpu_load_percent = 9;
}
```

**`CreateInstanceRequest`** — All sub-configs explicit:

```protobuf
message CreateInstanceRequest {
  string name = 1;
  string iso_path = 2;
  CpuConfig cpu = 3;
  MemoryConfig memory = 4;
  DiskConfig disk = 5;
  DisplayConfig display = 6;
  GpuConfig gpu = 7;
  NetworkConfig network = 8;
  FirmwareConfig firmware = 9;
  AudioConfig audio = 10;
  InputConfig input = 11;
}
```

**`oneof` types:**

- `RenderBackend`: `Venus` | `VirtioGpu` | `VirGl` | `Cpu` | `Passthrough(string gpu_pci_id)`
- `NetworkMode`: `Nat` | `Bridge(string interface)` | `Isolated`
- `InstanceKind`: `LinuxVm(string iso_path)` | `AndroidVm(AndroidProfile profile)`

**`UpdateInstanceConfigRequest`:**

```protobuf
message UpdateInstanceConfigRequest {
  string instance_id = 1;
  optional string name = 2;
  optional uint32 disk_size_gib = 3;
}
```

Protected fields (`id`, `kind`, `disk.path`) cannot be changed.
