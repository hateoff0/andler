# andler-cli

The `andler` binary — a thin gRPC client to `andlerd` via `andler-rpc`. No business logic here — all logic lives in `andler-daemon`/`andler-core`. Each subcommand makes one gRPC request and prints the response.

## Commands

### Instance Lifecycle

| Command | Description |
|---------|-------------|
| `andler create --file instance.toml` | Create LinuxVm or AndroidVm from TOML (auto-detected) |
| `andler create --kind linux --name <name> --iso-path <path> --disk-path <path> --ovmf-vars-template <path>` | Create LinuxVm with CLI flags |
| `andler create --kind android --name <name> --android-version <ver> --base-image-path <path> --ovmf-vars-template <path>` | Create AndroidVm with CLI flags |
| `andler create --instance <path>` | Create from TOML file via `--instance` flag |

### Create Flags (mutual exclusivity)

The following flags define how instance configuration is provided — they are **mutually exclusive**:

| Flags | Description |
|-------|-------------|
| `--file <path>` | Path to TOML instance file (shorthand for `--instance`) |
| `--instance <path>` | Path to TOML instance file |
| `--kind linux --name …` | All required LinuxVm fields (`--kind linux --name --iso-path --disk-path --ovmf-vars-template`) |
| `--kind android --name …` | All required AndroidVm fields (`--kind android --name --android-version --base-image-path --ovmf-vars-template`) |
| `--wizard` | Interactive wizard mode |

**TOML-only flags** (require `--file`/`--instance`):
- `--quick`: Skip wizard prompts, use defaults for optional fields
- `--dry-run`: Validate and preview without creating
- `--verify`: Run pre-flight checks (ISO/disk/OVMF existence, GPU memory, CPU/memory allocation)
Exit code: 0 if all checks passed, 1 if any failed — scriptable (`andler create --verify ... && andler create ...`).

**CLI-only flags** (require `--kind`):
- `--disk-size-gib <n>`: Initial disk size in GiB
- `--compact-on-shutdown`: Enable automatic disk compaction on graceful shutdown
- `--cdrom-bus <auto|virtio|ide>`: CD-ROM bus type
- `--no-uefi`: Disable UEFI, use BIOS/CSM boot
- `--overlay-size-gib <n>`: Disk size in GiB (Android only)
- `--linked-overlay`: Link the disk to the base image as a thin overlay instead of making a full independent copy (Android only). Default: off — full copy.
- `--gapps <true|false>`: Include Google Apps (Android only)
- `--microg <true|false>`: Include microG (Android only)
- `--arm-translator <libndk|libhoudini>`: ARM translation mode (Android only)
- `--instances-root <path>`: Custom instances root directory (Android only)

| `andler start <instance-id>` | Start an instance |
| `andler stop <instance-id> [--graceful]` | Stop an instance (default: force kill (SIGKILL); `--graceful`: graceful ACPI shutdown (SIGTERM)) |
| `andler pause <instance-id>` | Pause a running instance |
| `andler resume <instance-id>` | Resume a paused instance |
| `andler status <instance-id>` | Print current status |

### Information

| Command | Description |
|---------|-------------|
| `andler list [--full-id] [--state <state>] [--name <regex>] [--sort <key>] [--json]` | List instances. `--full-id`/`-q`: full UUID. `--state`: filter by state. `--name`: regex filter. `--sort`: `name`/`state`/`none` (default: `none`). `--json`: machine-readable. |
Default: UUIDs truncated to 8 characters (matching `docker ps`). Use `--full-id` / `-q` for full UUID.
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
| `andler logs <instance-id> [--source stdout|stderr] [--grep <regex>] [--tail <n>]` | Live-tail QEMU stdout/stderr. `--source`: filter stream. `--grep`: regex filter. `--tail`: backlog lines. |
`--tail <n>`: Shows last `n` lines. Uses a 300ms idle gap as a heuristic to detect the boundary between historical and live lines (andlerd sends both as one unbroken stream). A ring buffer holds `n` filtered lines while waiting for the gap; once observed, the buffer is flushed and switches to plain streaming.
| `andler metrics <instance-id> [--once] [--json]` | Stream resource metrics. `--once`: single sample and exit. `--json`: machine-readable output. |

### Snapshots

| Command | Description |
|---------|-------------|
| `andler snapshot <id> create --tag <name> [--description <text>] [--timeout <secs>]` | Create snapshot (requires Running/Paused). `--timeout` overrides instance default. |
| `andler snapshot <id> restore --tag <name> [--timeout <secs>]` | Restore from snapshot (requires Running/Paused). `--timeout` overrides instance default. |
| `andler snapshot <id> delete --tag <name> [--timeout <secs>]` | Delete snapshot (requires Running/Paused). `--timeout` overrides instance default. |
| `andler snapshot <id> list` | List all snapshots |

### Configuration & Editing

| Command | Description |
|---------|-------------|
| `andler edit <instance-id>` | Edit instance config in `$VISUAL`/`$EDITOR` as TOML (falls back to `vi`) |
| `andler wizard` | Launch interactive wizard (default when no subcommand given) |
| `andler doctor` | Check the local environment (KVM, QEMU, OVMF, nbd, sudoers, andlerd reachability, base images) — read-only, works even if andlerd isn't running |
| `andler completions <shell>` | Generate shell completion script (bash/zsh/fish) |

### Disk Management

| Command | Description |
|---------|-------------|
| `andler disk create <path> --size <size>` | Create a new empty qcow2 disk |
| `andler disk info <path>` | Show disk info (virtual size, actual usage, format, backing file) |
| `andler disk resize <path> --size <size> [--shrink]` | Resize an existing disk. `--shrink` required to confirm shrinking. |
| `andler disk compact <path>` | Compact a disk (reclaim unused space) |

Size format: `64GB`, `128000MB`, `1T`, `512000` (bytes). Case-insensitive.

### Guest Package Management

| Command | Description |
|---------|-------------|
| `andler guest install <package> <instance-id>` | Install a package in the guest OS (auto-fallback: online via QMP if running, offline via qemu-nbd if stopped) |
| `andler guest remove <package> <instance-id>` | Remove a package from the guest OS (auto-fallback) |
| `andler guest list <instance-id>` | List known packages and their status in the guest OS |
| `andler guest boot-mode <instance-id> [android\|linux]` | Get (no argument) or switch the guest's boot target on an Android VM's unified base image. Requires a restart to apply. |

Known packages: `spice-vdagent` (shared folders), `qemu-guest-agent` (host-guest communication), `spice-webdavd` (webdav shared folders).

## Daemon Address

Override with `--daemon-addr <url>` before the subcommand, or `ANDLERD_ADDR` env var. Default: `http://127.0.0.1:50051`.
**Environment variables**:
- `ANDLER_WIZARD_NOT_TTY`: When set, forces `is_tty()` to return false (test override for wizard).

## TOML Instance File Format

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

overlay_size_gib = 20
gapps = false
microg = false
arm_translator = "libndk"
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
# Off by default. Rewrites the whole disk file via qemu-img convert after
# every graceful shutdown to reclaim freed-up space — see API.md, "Disk management". Only takes effect for qcow2 disks.
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
ovmf_code_path = "/usr/share/edk2/x64/OVMF_CODE.4m.fd"
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

**`InstanceFileError`**: `Read { path, source }` | `Parse { path, source }` | `InvalidPath { path, reason }`.

**Path canonicalization**: `InstanceFile::load()` canonicalizes all path fields (relative paths become absolute from the TOML file's directory). Paths are validated and must exist (for required files) or be creatable (for disk paths).

**Legacy field precedence**: For Android VMs with `arm_translator`, the field accepts `libndk` as a legacy alias. When `arm_translator = "libndk"` is set, it maps to the Libndk translator. The valid values are `none`, `libndk`, and `libhoudini` — `hibridge` is not a valid value.

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

`andler metrics` streams key=value pairs:

```
cpu=12.34%    rss=1.23 GiB   disk_r=45.6 MB/s  disk_w=12.3 MB/s  net_rx=1.2 MB/s   net_tx=0.5 MB/s   vram=512 MiB       gpu=68%
```

GPU fields (vram, gpu) appear when AMD, NVIDIA, or Intel GPU data is available.

## Clone Modes

- **`linked`**: Cheap (no data copy), but source can't be purged while clones exist.
- **`full-standalone`**: Expensive (full copy), fully independent.
- **`shared-base`**: Byte-copy of source disk, survives source deletion, remains thin relative to shared base image.

`LinuxVm` supports `linked` and `full-standalone`. `SharedBase` is rejected (`SharedBaseNotSupportedForLinuxVm`).

## Snapshot Operations

- **Create**: Requires Running/Paused instance. Async QEMU job with configurable timeout.
- **Restore**: Requires Running/Paused instance. Reverts disk state.
- **Delete**: Requires Running/Paused instance. Idempotent.
- **List**: Any state. Shows tag, description, creation time.

## Modules

| Module | File | Purpose |
|--------|------|---------|
| `main.rs` | 769 lines | Clap CLI definition, gRPC client setup, subcommand dispatch |
| `create.rs` | 412 lines | `Create` command — builds gRPC request from CLI flags |
| `edit.rs` | 96 lines | `Edit` command — open config in `$EDITOR`, send changes to daemon |
| `disk.rs` | 77 lines | `Disk` command — create, info, resize, compact |
| `guest.rs` | 201 lines | `Guest` command — install/remove/list packages, boot-mode get/switch in guest OS |
| `status.rs` | 681 lines | `Status`, `List`, `Config`, `Logs`, `Metrics` commands |
| `snapshot.rs` | 109 lines | `Snapshot` command — create, restore, delete, list (with spinner) |
| `lifecycle.rs` | 124 lines | `Start`, `Stop`, `Pause`, `Resume`, `Remove` commands |
| `clone.rs` | 41 lines | `Clone`, `Export` commands |
| `instance_file.rs` | 805 lines | TOML instance file parser |
| `helpers.rs` | 366 lines | `parse_size`, `format_size`, `format_bytes`, `ensure_qcow2_extension` |
| `wizard/mod.rs` | 776 lines | Interactive wizard entry point, orchestration, `build_create_request()` |
| `wizard/basic.rs` | 303 lines | Basic mode: kind, name, ISO, disk questions |
| `wizard/advanced.rs` | 604 lines | Advanced mode: 16 hardware questions with auto-detection defaults |
| `wizard/summary.rs` | 282 lines | Summary display, Create/Modify/Cancel actions |
| `verify.rs` | 11.0KB | `--verify` flag on create — pre-flight checks (ISO, disk, OVMF, GPU memory, CPU/memory) |
| `preview.rs` | 6.5KB | `--dry-run` flag — resolves and prints what would be created including QEMU command line |
