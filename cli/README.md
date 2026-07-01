# andler-cli

The `andler` binary — a thin gRPC client to `andlerd` via `andler-rpc`. No business logic here — all logic lives in `andler-daemon`/`andler-core`. Each subcommand makes one gRPC request and prints the response.

## Commands

### Instance Lifecycle

| Command | Description |
|---------|-------------|
| `andler create --file instance.toml` | Create LinuxVm or AndroidVm from TOML (auto-detected) |
| `andler create --kind linux --name <name> --iso-path <path> --disk-path <path> --ovmf-vars-template <path>` | Create LinuxVm with CLI flags |
| `andler create --kind android --name <name> --android-version <ver> --base-image-path <path> --ovmf-vars-template <path>` | Create AndroidVm with CLI flags |
| `andler start <instance-id>` | Start an instance |
| `andler stop <instance-id> [--graceful]` | Stop an instance (without `--graceful`, kills immediately) |
| `andler pause <instance-id>` | Pause a running instance |
| `andler resume <instance-id>` | Resume a paused instance |
| `andler status <instance-id>` | Print current status |

### Information

| Command | Description |
|---------|-------------|
| `andler list` | List all registered instances (id / state / name) |
| `andler config <instance-id>` | Print full instance configuration (all 9 sections) |

### Lifecycle Management

| Command | Description |
|---------|-------------|
| `andler remove <instance-id> [--purge]` | Remove instance record. With `--purge`, also deletes disk + OVMF vars copy. |
| `andler clone <source-id> --name <new-name> --mode <linked\|full-standalone\|shared-base>` | Clone an instance |
| `andler export <source-id> <dest-path>` | Export instance disk as standalone file |

### Monitoring

| Command | Description |
|---------|-------------|
| `andler logs <instance-id>` | Live-tail QEMU stdout/stderr (no history, starts from connection) |
| `andler metrics <instance-id>` | Stream resource metrics (CPU%, RAM, disk I/O, net I/O, GPU) every second |

### Snapshots

| Command | Description |
|---------|-------------|
| `andler snapshot <id> create --tag <name> [--description <text>] [--timeout <secs>]` | Create snapshot (requires Running/Paused). `--timeout` overrides instance default. |
| `andler snapshot <id> restore --tag <name> [--timeout <secs>]` | Restore from snapshot (requires Stopped). `--timeout` overrides instance default. |
| `andler snapshot <id> delete --tag <name> [--timeout <secs>]` | Delete snapshot (requires Stopped). `--timeout` overrides instance default. |
| `andler snapshot <id> list` | List all snapshots |

### Disk Management

| Command | Description |
|---------|-------------|
| `andler disk create --path <path> --size <size>` | Create a new empty qcow2 disk |
| `andler disk info <path>` | Show disk info (virtual size, actual usage, format, backing file) |
| `andler disk resize <path> --size <size>` | Resize an existing disk |
| `andler disk compact <path>` | Compact a disk (reclaim unused space) |

Size format: `64GB`, `128000MB`, `1T`, `512000` (bytes). Case-insensitive.

## Daemon Address

Override with `--daemon-addr <url>` before the subcommand, or `ANDLERD_ADDR` env var. Default: `http://127.0.0.1:50051`.

## TOML Instance File Format

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
# Off by default. Rewrites the whole disk file via qemu-img convert after
# every graceful shutdown to reclaim freed-up space — see PLAN.md, "Disk
# management". Only takes effect for qcow2 disks.
compact_on_shutdown = false
# "auto" (default) picks virtio-scsi for known Linux distros (by ISO
# filename) and ide otherwise; can be forced to "virtio" or "ide".
cdrom_bus = "auto"

[cpu]
cores = 8
sockets = 1
threads = 2
affinity = []
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

Any section can be omitted entirely — then `reference_default()` fills it in. If a section is present, it must be complete (no partial overrides within a section).

### Headless Mode (no GPU/audio/X server)

For servers, CI, or Docker:

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

`display_engine = "None"` = `-display none`, no X11/Wayland access at all.

## TOML Parsing (`instance_file.rs`)

**`InstanceFile`**: TOML mirror of `CreateInstanceRequest` / `CreateAndroidInstanceRequest`. Fields that overlap with domain types (`cpu`, `memory`, etc.) reuse `andler_core::config::*` directly via `Deserialize` — not separate CLI-specific copies.

- `InstanceFile::load(path)`: Reads and parses TOML file.
- `InstanceFile::into_result()`: Returns `InstanceFileResult::Linux(req)` or `InstanceFileResult::Android(req)`, auto-detected from TOML content.

**`InstanceFileResult`**: `Linux(CreateInstanceRequest)` | `Android(CreateAndroidInstanceRequest)`.

**`InstanceFileError`**: `Read { path, source }` | `Parse { path, source }`.

### Tests

- Minimal Linux file parses with all defaults
- Custom disk size honored
- Android file with all options
- Android 11 version parses
- Auto-detect: `android_version` field → Android mode
- Auto-detect: `base_image_path` field → Android mode
- Auto-detect: no Android fields → Linux mode
- Missing required Linux field fails
- Missing file → `Read` error
- Invalid TOML → `Parse` error

## Metrics Output Format

`andler metrics` streams a table:

```
  CPU%   RAM        Disk R     Disk W     Net RX     Net TX     VRAM       GPU%
 12.34   1.23 GiB   45.6 MB/s  12.3 MB/s  1.2 MB/s   0.5 MB/s   512 MiB    67.8%
 11.20   1.24 GiB   44.1 MB/s  11.9 MB/s  1.1 MB/s   0.4 MB/s   513 MiB    68.2%
```

GPU columns (VRAM, GPU%) appear when AMD, NVIDIA, or Intel GPU data is available.

## Clone Modes

- **`linked`**: Cheap (no data copy), but source can't be purged while clones exist.
- **`full-standalone`**: Expensive (full copy), fully independent.
- **`shared-base`**: Byte-copy of source disk, survives source deletion, remains thin relative to shared base image.

`LinuxVm` supports `linked` and `full-standalone`. `SharedBase` is rejected (`SharedBaseNotSupportedForLinuxVm`).

## Snapshot Operations

- **Create**: Requires Running/Paused instance. Async QEMU job with configurable timeout.
- **Restore**: Requires Stopped instance. Reverts disk state.
- **Delete**: Requires Stopped instance. Idempotent.
- **List**: Any state. Shows tag, description, creation time.
