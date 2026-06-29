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
  [--disk-size-gib 100]

# AndroidVm via CLI
andler create \
  --kind android \
  --name my-android \
  --android-version 13 \
  --base-image-path /path/to/base.qcow2 \
  --ovmf-vars-template /path/to/VARS.fd \
  [--root magisk --magisk-dir /path/to/magisk/] \
  [--gapps] [--microg] [--libndk] \
  [--overlay-size-gib 20] \
  [--instances-root /path/to/instances/]
```

**Flags:**

| Flag | Required | Description |
|------|----------|-------------|
| `--file <path>` | Yes* | Path to TOML config file (*mutually exclusive with `--kind`) |
| `--kind <type>` | Yes* | VM type: `linux` or `android` (*mutually exclusive with `--file`) |
| `--name <name>` | Yes** | Instance name (**required in CLI mode) |
| `--ovmf-vars-template <path>` | Yes** | Path to OVMF_VARS template (**required in CLI mode) |
| `--iso-path <path>` | Yes*** | Path to installer ISO (***required for `--kind linux`) |
| `--disk-path <path>` | Yes*** | Path to disk file (***required for `--kind linux`) |
| `--disk-size-gib <size>` | No | Disk size in GiB (default: 40, Linux only) |
| `--android-version <ver>` | Yes**** | Android version: `11` or `13` (****required for `--kind android`) |
| `--base-image-path <path>` | Yes**** | Path to Android base image qcow2 (****required for `--kind android`) |
| `--gapps` | No | Include Google Apps |
| `--microg` | No | Include microG |
| `--libndk` | No | Include ARM→x86 translation (libhoudini/libndk) |
| `--root <mode>` | No | Root mode: `none` (default), `magisk` |
| `--magisk-dir <path>` | No | Path to Magisk binaries (required when `--root magisk`) |
| `--overlay-size-gib <size>` | No | Overlay disk size in GiB (default: 20, Android only) |
| `--instances-root <path>` | No | Instance directory root (default: `~/.local/share/andler/instances`) |

### `start`

```bash
andler start <instance-id>
```

Transitions: Created → Starting → Running. Fails if instance is already running or in a non-terminal error state.

### `stop`

```bash
andler stop <instance-id> [--graceful]
```

Without `--graceful`: kills the QEMU process immediately (SIGKILL).
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
andler list
```

Prints all registered instances:

```
INSTANCE_ID                          STATE     NAME
550e8400-e29b-41d4-a716-446655440000 Running   my-android
6ba7b810-9dad-11d1-80b4-00c04fd430c8 Stopped   my-linux-vm
```

State is the daemon's record, not live backend status. Use `status` for exact state of a specific instance.

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

Source must be stopped. `LinuxVm` supports `linked` and `full-standalone`. `SharedBase` is rejected for `LinuxVm` (`SharedBaseNotSupportedForLinuxVm`).

Cloning a clone is allowed.

### `export`

```bash
andler export <source-id> <dest-path>
```

Exports the instance disk as a standalone file at `dest_path`. Does not create a new instance. Source must be stopped.

### `logs`

```bash
andler logs <instance-id>
```

Live-tails QEMU stdout/stderr with `[stdout]`/`[stderr]` prefix. No historical output — starts from connection time.

If instance has no running backend (never started, or already stopped), prints a warning to stderr and exits with code 0.

### `metrics`

```bash
andler metrics <instance-id>
```

Streams resource metrics every second:

```
  CPU%   RAM        Disk R     Disk W     Net RX     Net TX     VRAM       GPU%
 12.34   1.23 GiB   45.6 MB/s  12.3 MB/s  1.2 MB/s   0.5 MB/s   512 MiB    67.8%
 11.20   1.24 GiB   44.1 MB/s  11.9 MB/s  1.1 MB/s   0.4 MB/s   513 MiB    68.2%
```

GPU columns (VRAM, GPU%) appear when AMD, NVIDIA, or Intel GPU data is available.

### `snapshot`

```bash
# Create (requires Running/Paused instance)
andler snapshot <instance-id> create --tag before-update --description "Pre-upgrade state" --timeout 120

# Restore (requires Stopped instance)
andler snapshot <instance-id> restore --tag before-update --timeout 10

# Delete (requires Stopped instance)
andler snapshot <instance-id> delete --tag before-update --timeout 5

# List (any state)
andler snapshot <instance-id> list
```

The `--timeout` flag overrides the per-instance `snapshot_timeout_secs` for a single operation. If not specified, uses the instance default (30s).

### `disk`

Disk management commands — wraps `qemu-img` operations.

```bash
# Create a new empty disk
andler disk create --path /path/to/disk.qcow2 --size 64GB
andler disk create --path /path/to/disk.qcow2 --size 128000MB
andler disk create --path /path/to/disk.qcow2 --size 1T

# Show disk information
andler disk info /path/to/disk.qcow2

# Resize an existing disk
andler disk resize /path/to/disk.qcow2 --size 80GB

# Compact a disk (reclaim unused space)
andler disk compact /path/to/disk.qcow2
```

**Size format**: Supports `GB`, `GiB`, `MB`, `MiB`, `TB`, `TiB` (case-insensitive). Space between number and unit is optional. Plain number = bytes.

| Command | Description |
|---------|-------------|
| `create --path <path> --size <size>` | Create a new empty qcow2 disk |
| `info <path>` | Show disk info (virtual size, actual usage, format, backing file) |
| `resize <path> --size <size>` | Resize an existing disk |
| `compact <path>` | Compact a disk (reclaim unused space via `qemu-img convert`) |

## Instance TOML File

### Linux VM (minimal)

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.local/share/andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.local/share/andler/my-linux-vm/VARS.fd"
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
instances_root = "/home/user/.local/share/andler/instances"
```

### Linux VM (full example)

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.local/share/andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.local/share/andler/my-linux-vm/VARS.fd"

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

[input]
tablet_mode = true
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
| `disk` | 40 GiB qcow2, no backing, thin provisioning, discard |
| `display` | 1920x1080, 96 DPI, no limit, SDL, no fullscreen |
| `gpu` | Venus, 4096 MiB hostmem, blob+gl on |
| `network` | NAT, virtio-net-pci |
| `firmware` | OVMF_CODE at `/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd` |
| `audio` | PipeWire |
| `input` | tablet+hide_cursor+clipboard all true |

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
| `ListInstances` | `Empty` | `ListInstancesResponse` | Unary |
| `RemoveInstance` | `RemoveInstanceRequest` | `Empty` | Unary |
| `GetInstanceConfig` | `InstanceIdRequest` | `GetInstanceConfigResponse` | Unary |
| `StreamInstanceLogs` | `InstanceIdRequest` | `stream LogLineResponse` | Server-streaming |
| `StreamResourceMetrics` | `InstanceIdRequest` | `stream ResourceMetricsResponse` | Server-streaming |
| `CloneInstance` | `CloneInstanceRequest` | `CreateInstanceResponse` | Unary |
| `ExportInstanceDisk` | `ExportInstanceDiskRequest` | `ExportInstanceDiskResponse` | Unary |
| `CreateSnapshot` | `CreateSnapshotRequest` | `CreateSnapshotResponse` | Unary |
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary |
| `ListSnapshots` | `InstanceIdRequest` | `ListSnapshotsResponse` | Unary |

### Error Codes

| gRPC Status | Daemon Error | When |
|-------------|--------------|------|
| `NOT_FOUND` | `InstanceNotFound` | Unknown instance ID |
| `UNIMPLEMENTED` | `NoBackendRegistered` | Backend kind not available |
| `FAILED_PRECONDITION` | `InvalidTransition`, `InstanceNotRemovable`, `InstanceNotClonable`, `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`, snapshot state errors | Wrong lifecycle state |
| `ALREADY_EXISTS` | `SnapshotAlreadyExists` | Duplicate snapshot tag |
| `INVALID_ARGUMENT` | `ConvertError` | Malformed request |
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
