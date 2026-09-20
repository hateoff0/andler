# andler-cli

The `andler` binary — a thin gRPC client to `andlerd` via `andler-rpc`. No business logic here — all logic lives in `andler-daemon`/`andler-core`. Each subcommand makes one gRPC request and prints the response.

## Commands

### Instance Lifecycle

| Command | Description |
|---------|-------------|
| `andler create --file instance.toml` | Create LinuxVm or AndroidVm from TOML (auto-detected) |
| `andler create --kind linux --name <name> --iso-path <path> --disk-path <path>` | Create LinuxVm with CLI flags |
| `andler create --kind android --name <name> --android-version <ver>` | Create AndroidVm with CLI flags (base image auto-discovered) |

### Create Flags (mutual exclusivity)

The following flags define how instance configuration is provided — they are **mutually exclusive**:

| Flags | Description |
|-------|-------------|
| `--file <path>` | Path to TOML instance file (auto-detects LinuxVm/AndroidVm; mutually exclusive with `--kind`) |
| `--kind <linux\|android>` | VM type for CLI-mode creation (requires the type's required flags below) |
| `--quick` | Skip the wizard and create with defaults (requires `--kind`; mutually exclusive with `--file`). Guest-side selections recorded by the defaults (ARM translator, clipboard agent) are **not** installed — run `andler guest apply <id>` for that |

**Mode selection**: exactly one of `--file`, `--kind`, or neither (bare `andler create` starts the interactive wizard).

**Linux CLI mode** (`--kind linux`): `--name`, `--iso-path`, `--disk-path` required. Optional: `--disk-size-gib` (≥1, default 256), `--compact-on-shutdown`, `--cdrom-bus <auto|virtio|ide>` (default auto), `--no-uefi`, `--ovmf-vars-template` (auto-detected when omitted).

**Android CLI mode** (`--kind android`): `--name`, `--android-version <11|13>` required. Optional: `--base-image-path` (omitted → daemon auto-discovers in `~/.andler/cache/base-images/`), `--gapps`, `--microg`, `--arm-translator <none|libndk|libhoudini>`, `--overlay-size-gib` (≥1, default 128), `--linked-overlay`, `--instances-root`, `--ovmf-vars-template`.

**Validation flags** (both modes):
- `--dry-run`: Validate and preview the resolved config + QEMU command line without creating (doesn't contact the daemon)
- `--verify`: Run pre-flight checks (ISO/disk/OVMF existence, GPU memory, CPU/memory allocation). Exit code: 0 if all checks passed, 1 if any failed — scriptable (`andler create --verify ... && andler create ...`)

| `andler start <instance-id>` | Start an instance |
| `andler stop <instance-id> [--graceful]` | Stop an instance (default: force kill (SIGKILL); `--graceful`: graceful ACPI shutdown (SIGTERM)) |
| `andler pause <instance-id>` | Pause a running instance |
| `andler resume <instance-id>` | Resume a paused instance |
| `andler status <instance-id> [--json]` | Print current state and the guest readiness level (`<reached> of <terminal>`, see [ARCHITECTURE.md](../../docs/ARCHITECTURE.md)). `--json`: JSON object with state/detail/error_message/readiness/terminal_readiness |

### Information

| Command | Description |
|---------|-------------|
| `andler list [--full-id] [--state <state>] [--name <regex>] [--sort <key>] [--json]` | List instances. `--full-id`/`-q`: full UUID. `--state`: filter by state. `--name`: regex filter. `--sort`: `name`/`state`/`none` (default: `none`). `--json`: machine-readable. |
Default: UUIDs truncated to 12 characters (matching `docker ps`). Use `--full-id` / `-q` for full UUID.
| `andler config view <instance-id>` | Print full instance configuration (all 9 sections) |
| `andler config edit <instance-id>` | Open the real `instance.toml` in `$VISUAL`/`$EDITOR` (fallback `vi`/`vim`/`nano`), apply edits via gRPC |
| `andler config set <instance-id> <key> <value>` | Update a single config key. The full `InstanceConfig` key schema is walked: every key is either settable or rejected with the reason it is not (`cpu.affinity` → edit `instance.toml` directly). `display.resolution` applies live to a running guest and persists via fw_cfg |

### Lifecycle Management

| Command | Description |
|---------|-------------|
| `andler remove <instance-id> [--purge]` | Remove instance record. With `--purge`, also deletes disk + OVMF vars copy (confirms on TTY). |
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
| `andler snapshot create <id> --tag <name> [--description <text>] [--timeout <secs>]` | Create an **external overlay** snapshot, live: the QEMU block graph is switched to a fresh overlay and the previous disk becomes a layer under `disk.snapshots/` (requires Running/Paused). Free space is pre-checked. `--timeout` overrides the instance default. |
| `andler snapshot restore <id> --tag <name> [--branch] [--timeout <secs>] [--idempotency-token <k>]` | Restore **offline** (instance must be Created/Stopped). Default discards layers newer than the target; `--branch` archives the current chain as a branch you can switch back to. |
| `andler snapshot delete <id> --tag <name> [--timeout <secs>]` | Delete a layer **offline** (Created/Stopped): commits it into its parent and re-points its children. |
| `andler snapshot list <id>` | List all snapshots (human-readable timestamps) |
| `andler snapshot --json list <id>` | List all snapshots as a JSON array |

### Configuration & Editing

| Command | Description |
|---------|-------------|
| `andler config edit <instance-id>` | Open the real `instance.toml` in `$VISUAL`/`$EDITOR` (fallback `vi`/`vim`/`nano`) and apply edits via gRPC |
| `andler wizard` | Launch interactive wizard (default when no subcommand given) |
| `andler doctor` | Check the local environment (KVM, QEMU, OVMF, `CAP_NET_ADMIN`, the zero-root offline prerequisite — `guestfish` — andlerd reachability, base images) — read-only, works even if andlerd isn't running |
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
| `andler guest install <package> <instance-id>` | Install a package in the guest OS. Online via the QGA guest-agent socket when the VM runs; a stopped VM is auto-started headless for maintenance and stopped again. `--offline` forces the zero-root libguestfs appliance path. Special case: `libndk`/`libhoudini`/`none` route to the ARM-translator switcher instead (`none` disables ARM translation — no download; `--translator-dir <path>` points at a local extracted cache) |
| `andler guest remove <package> <instance-id>` | Remove a package from the guest OS (auto-fallback). A translator name (`libndk`/`libhoudini`/`none`) switches the ARM translator to `none` instead |
| `andler guest list <instance-id>` | List known packages and their status in the guest OS |
| `andler guest boot-mode <instance-id> [android\|linux]` | Get (no argument) or switch the guest's boot target on an Android VM's unified base image. Requires a restart to apply. |
| `andler guest apply <instance-id> [--json]` | Apply the guest-side work the instance's own config selects: the ARM translator when `kind.android_profile.arm_translator` is not `none`, `spice-vdagent` when `input.clipboard_enabled`. Offline through the libguestfs appliance while the disk is idle, online through the guest agent on a running VM; one classified outcome per selection (`applied`/`already present`/`skipped`/`failed`) with the retry command in the message. This is what the wizard runs right after creating a VM. |

Known packages: `spice-vdagent` (shared folders), `qemu-guest-agent` (host-guest communication), `spice-webdavd` (webdav shared folders).

## Base Images

| Command | Description |
|---------|-------------|
| `andler image list [--android-version <11\|13>] [--variant <vanilla\|gapps>] [--json]` | List the base images published by the project's release pipeline (GitHub releases), newest build per Android version + package set, with the download size and whether each one is already cached locally. `--json` reports `{source, images:[{id, android_major, android_variant, built_at, release_tag, download_bytes, installed_bytes, installed, installed_path}]}` |
| `andler image download (--android-version <11\|13> --variant <vanilla\|gapps> \| --release-tag <tag>) [--force] [--json]` | Download, verify and install one published build into `~/.andler/cache/base-images/<android>-<variant>/`. Every `.part` asset is checked against the sha256 in the release manifest and the unpacked image against the manifest's own sha256 before anything is installed; an already-cached build is reused unless `--force`. `--json` streams one progress document per line (`phase`, `asset`, `asset_index`/`asset_count`, `downloaded_bytes`/`total_bytes`, `installed_path`). `ANDLERD_IMAGE_REPO`/`ANDLERD_IMAGE_API_BASE` on the daemon point at another source |

`andler cache list`/`cache clean` (Configuration & Editing) work on whatever is in that cache, downloaded or locally built.

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

overlay_size_gib = 128
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
priority = "high"

[memory]
size_bytes = 8589934592
ballooning = false
zram = false
ksm = true

[display]
resolution = { width = 1920, height = 1080 }
dpi = 96
fps_limit = 0
engine = "sdl"
fullscreen = false

[gpu]
render_backend = "venus"
hostmem_bytes = 4294967296
blob = true
gl = true

[network]
mode = "nat"
device_model = "virtio-net-pci"

[firmware]
ovmf_code_path = "/usr/share/edk2/x64/OVMF_CODE.4m.fd"
ovmf_vars_path = "/path/to/VARS.fd"

[audio]
backend = "pipewire"

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
render_backend = "cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
engine = "none"
fullscreen = false

[audio]
backend = "none"
```

`engine = "none"` = `-display none`, no X11/Wayland access at all.

## TOML Parsing (`instance_file.rs`)

**`InstanceFile`**: TOML mirror of `CreateInstanceRequest` / `CreateAndroidInstanceRequest`. Fields that overlap with domain types (`cpu`, `memory`, etc.) reuse `andler_core::config::*` directly via `Deserialize` — not separate CLI-specific copies.

- `InstanceFile::load(path)`: Reads and parses TOML file.
- `InstanceFile::into_result()`: Returns `InstanceFileResult::Linux(req)` or `InstanceFileResult::Android(req)`, auto-detected from TOML content.

**`InstanceFileResult`**: `Linux(CreateInstanceRequest)` | `Android(CreateAndroidInstanceRequest)`.

**`InstanceFileError`**: `Read { path, source }` | `Parse { path, source }` | `InvalidPath { path, reason }`.

**Path canonicalization**: `InstanceFile::load()` canonicalizes all path fields (relative paths become absolute from the TOML file's directory). Paths are validated and must exist (for required files) or be creatable (for disk paths).

**`arm_translator` values**: `none`, `libndk`, and `libhoudini` — `hibridge` is not a valid value. The field also accepts the bare `libndk` flag name as an alias for the Libndk translator.

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
- Wizard: `--quick` request building for Linux/Android (incl. base-image resolution failures and `--linked-overlay` forwarding), non-TTY refusal, request builders per mode, `AdvancedConfig::default` = the recommended configuration, network choice ↔ mode mapping (incl. the bridge interface staying with its own question), Android group labels unique
- Wizard answers: name (trimmed, separators rejected) and disk-size validators incl. range boundaries, resolution parsing, and the base-image download's per-phase progress line (batch position only when the daemon reports one)
- Wizard presentation: panels render equal-width frames regardless of how long a base-image path is, `NO_COLOR` disables styling, summary text carries the answers and lists what will be installed inside the VM
- Image command: the release-catalog JSON shape and progress-line rendering

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
| `main.rs` | — | Clap CLI definition, gRPC client setup, subcommand dispatch |
| `create.rs` | — | `Create` command — builds gRPC request from CLI flags |
| `edit.rs` | — | `Edit` command — open config in `$EDITOR`, send changes to daemon |
| `disk.rs` | — | `Disk` command — create, info, resize, compact |
| `image.rs` | — | `Image` command — list published base images, download+verify one into the cache |
| `guest.rs` | — | `Guest` command — install/remove/list packages, `apply` (config-driven guest selections), boot-mode get/switch in guest OS |
| `status.rs` | — | `Status`, `List`, `Config`, `Logs`, `Metrics` commands |
| `snapshot.rs` | — | `Snapshot` command — create, restore, delete, list (with spinner) |
| `lifecycle.rs` | — | `Start`, `Stop`, `Pause`, `Resume`, `Remove` commands |
| `clone.rs` | — | `Clone`, `Export` commands |
| `instance_file.rs` | — | TOML instance file parser |
| `helpers.rs` | — | `parse_size`, `format_size`, `format_bytes`, `ensure_qcow2_extension`, `spinner`, `download_bar`/`download_bar_message`/`update_download_bar` (the shared base-image download bar, used by `image download` and the wizard), `short_id`, `emit_json` |
| `wizard/mod.rs` | — | Wizard orchestration: `run` (answers → request), `--quick` defaults, `handle_wizard` (create + apply + report), errors |
| `wizard/basic.rs` | — | Basic questions: kind, name, ISO, disk size, UEFI, Android version/package set |
| `wizard/advanced.rs` | — | Advanced questions grouped by area (boot/disks, display/GPU, devices, CPU/memory, network, Android); "change some settings" re-asks the picked groups, each question defaulting to its previous answer |
| `wizard/build.rs` | — | Config building — wizard answers → `CreateInstanceRequest`/`CreateAndroidInstanceRequest`, base-image re-resolution |
| `wizard/ui.rs` | — | Presentation — content-sized panels with NO_COLOR-aware styling, plus the dialog's framing: session intro/outro, step and warning lines, and the typed prompts' shared error mapping |
| `wizard/base_image.rs` | — | Base-image question: local cache lookup, offering a published build for download, manual path |
| `wizard/apply.rs` | — | Create + apply the guest selections + report (instance id, per-selection outcome, next steps) |
| `wizard/summary.rs` | — | Summary screen (identity/storage/display/CPU/network + what will be installed inside the VM) and Create/Change/Cancel |
| `verify.rs` | — | `--verify` flag on create — pre-flight checks (ISO, disk, OVMF, GPU memory, CPU/memory) |
| `preview.rs` | — | `--dry-run` flag — resolves and prints what would be created including QEMU command line |
