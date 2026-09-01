# API Reference

## CLI Commands

All commands connect to the daemon via gRPC. Pass `--daemon-addr <url>` before the subcommand to target a non-default daemon.

### Output formats

Every subcommand accepts `--json` (where it produces structured output) to emit a JSON document on **stdout** instead of human-readable text. The exact shape is given per subcommand above; the common success shapes are `{"instance_id": "<id>"}` (create/clone/export) and per-subcommand objects/arrays.

Errors are unified: on any failure the CLI prints a single JSON document to **stderr** — `{"error": "<message>"}` — when `--json` was requested, and a plain actionable message to stderr otherwise. The message is the same in both cases (sanitized, truncated to 384 chars, and actionable — it says what to do next rather than echoing raw QEMU/guest-agent stderr). Exit code is non-zero on failure in both cases.

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
| `--ovmf-vars-template <path>` | No | Path to OVMF_VARS template (auto-detected when omitted) |
| `--iso-path <path>` | Yes*** | Path to installer ISO (***required for `--kind linux`) |
| `--disk-path <path>` | Yes*** | Path to disk file (***required for `--kind linux`) |
| `--disk-size-gib <size>` | No | Disk size in GiB, must be ≥ 1 (default: 256, Linux only) |
| `--cdrom-bus <bus>` | No | CD-ROM bus: `auto` (default), `virtio`, `ide` (Linux only) |
| `--compact-on-shutdown` | No | Auto-compact disk after shutdown (Linux only) |
| `--android-version <ver>` | Yes**** | Android version: `11` or `13` (****required for `--kind android`) |
| `--base-image-path <path>` | No | Android base image qcow2; when omitted the daemon auto-discovers the freshest matching image in `~/.andler/cache/base-images/` (flat cache root or `android<version>-<variant>/` subdirectories) |
| `--gapps` | No | Include Google Apps |
| `--microg` | No | Include microG |
| `--arm-translator <mode>` | No | ARM→x86 translation: `none` (default), `libndk`, `libhoudini` |
| `--overlay-size-gib <size>` | No | Overlay disk size in GiB (default: 128, Android only) |
| `--linked-overlay` | No | Use a linked (backing-file) overlay instead of a standalone copy (Android only) |
| `--template <name>` | No | VM template applied over the defaults and under the CLI flags (merge order: defaults < template < flags). Built-ins: `headless` (CPU renderer, no display/audio) and `desktop` (Venus GPU, SDL display, audio). User templates live in `~/.andler/templates/<name>.toml` and accept the same partial sections. Only supported with `--kind linux` in this phase. |
| `--json` | Output as a JSON object: `{"instance_id": "<id>"}` for a real create; with `--dry-run --json`, the fully resolved `InstanceConfig` (all config sections) serialized as JSON; with `--verify --json`, a `{"name","passed","checks":[...]}` report (see below) |

On success both file and CLI modes print `Created instance <name> (<id>)`. TOML mode requires `disk_path` and `iso_path`; a missing field is a clean error (`missing required field disk_path in instance file`), and an unknown `android_version` is rejected (`unsupported value for android_version: 12`).

With `--verify --json`, the output is a single JSON object: `{"name": <instance name>, "passed": <bool>, "checks": [{"name": <check name>, "ok": <bool>, "detail": <human-readable string>}]}`. `passed` is `true` only when every check passes. Exit code is `1` on failure (nothing was created) and `0` on success, identical to the text mode.

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

With `--json`, each transition prints the post-transition status `{ "instance_id", "state", "detail", "error_message" }` (all four fields always present; `error_message` is empty when the transition had no error).

### `status`

```bash
andler status <instance-id> [--json]
```

Prints current state: `Created`, `Starting`, `Running`, `Paused`, `Stopping`, `Stopped`, or `Error`.

| Flag | Description |
|------|-------------|
| `--json` | Output as a JSON object: `{ "instance_id", "state", "detail", "error_message" }` (`error_message` omitted when empty) |

### `list`

```bash
andler list [--state <STATE>] [--name <REGEX>] [--sort <FIELD>] [--json] [--full-id]
```

Lists all registered instances. Supports filtering by state and name (regex, case-insensitive). A directory under `~/.andler/instances/` whose `instance.toml` is missing, unreadable, or invalid is reported as a broken entry (`[broken: reason]` after the name, `broken_reason` in JSON) — it is held in memory as `Error` and can be removed with `andler remove <id> --purge`.

| Flag | Description |
|------|-------------|
| `--state <STATE>` | Filter by state: `Created`, `Starting`, `Running`, `Paused`, `Stopping`, `Stopped`, `Error` |
| `--name <REGEX>` | Filter by instance name (regular expression, case-insensitive) |
| `--sort <FIELD>` | Sort by: `none` (default), `name`, `state` |
| `--json` | Output as JSON array |
| `--full-id` / `-q` | Show the full 64-char ID instead of the 12-char short ID |

### `cache`

Base-image cache management — Android base images live under `$ANDROID_HOME/cache/base-images/` (`ANDROID_HOME` defaults to `~/.andler`).

```bash
# List every cached base-image build
andler cache list
andler cache --json list

# Remove superseded builds and orphan manifests/qcow2s
andler cache clean [--dry-run] [--json]
```

`list` prints one line per build as `{id}  {qcow2_path}`, where `id` is the manifest id (`android<major>-<variant>-<built_at>`).

`clean` removes, by policy:

- **Superseded builds** — every Android build older than the freshest for its `(android_major, android_variant)` group. Only one build per `(major, variant)` survives; older ones are removed.
- **Orphan manifests** — `*.manifest.json` files with no matching `*.qcow2` next to them.
- **Orphan qcow2s** — `*.qcow2` files with no matching `*.manifest.json`.

`--dry-run` reports what would be removed without deleting anything; without it, the files are deleted. `--json` emits `{ "cache_dir", "dry_run", "removed": [{ "qcow2", "manifest", "label" }] }` — each entry names the removed file(s) and a reason (`android<major>-<variant>-<built_at>` for a superseded build, or `orphan manifest (no matching qcow2)` / `orphan qcow2 (no manifest)`).

`clean` is idempotent: a cache with a single build per `(major, variant)` and no orphans removes nothing.

Covered by the `10_base_images.sh` e2e suite.

### `config`

```bash
andler config view <instance-id>
andler config edit <instance-id>
andler config set <instance-id> <key> <value>
andler config status <instance-id>
andler config --instance <instance-id> [--edit]
```

`view` prints the full instance configuration (all 9 sections + hotplugged device lists + metadata). Output format is human-readable but not valid TOML.

`edit` opens the real on-disk `instance.toml` in `$VISUAL`/`$EDITOR` (falling back to `vi`/`vim`/`nano`), then applies the edited config via gRPC. If the TOML is invalid, nothing is applied and the error message points at the file to fix. `config edit` requires the CLI to run on the same machine as the daemon (it edits the file on disk). Prints `No changes made.` when the file is left untouched.

`status` reports how `instance.toml` and the daemon's loaded config relate: the instance id/name/state, the list of keys where file and memory differ (or `file and memory are in sync`), the live-applied resolution, and any error from reading the file. On an idle instance (`Created`/`Stopped`/`Error`) a hand-edited file is applied on read, so `status` normally shows sync; on a `Running`/`Paused` instance manual edits are never applied silently — they appear as a pending diff and take effect at the next stop/start or via `config set`/`edit`.

`config status --json` prints `{"instance_id", "name", "state", "live_resolution", "file_error", "diffs"}` (`live_resolution` and `file_error` are `null` when absent; `diffs` is an array of `{"key", "file_value", "memory_value"}`).

`set` updates a single config key by name. Supported keys (the full whitelist lives in `andler-core` `config_keys()`):

| Key | Value | Requirements |
|-----|-------|--------------|
| `display.resolution` | `WxH`, e.g. `1920x1080` | Works in any state; on a `Running`/`Paused` instance the new resolution is pushed into the guest over the QEMU guest agent immediately (applied by the guest compositor/session), and persisted for the next boot (delivered via fw_cfg) |
| `name` | any valid instance name | Instance must be stopped (`disk_idle`) |
| `autostart` | `true`\|`false` | Works in any state; takes effect at the next daemon restart |
| `arm_translator` (`kind.android_profile.arm_translator`) | `none` \| `libndk` \| `libhoudini` | Android instances only, instance must be stopped; performs the same offline switch as `SwitchArmTranslator` |
| `cpu.affinity` | read-only | Immutable via `config set`: pin the VM's threads to host CPUs by editing `affinity = [0, 2, 4, 6]` in `instance.toml` — the QEMU process is `taskset`-pinned to that set, and starting an instance whose pin overlaps a running pinned instance is refused (`host CPU N is pinned by running instance …`) |
| `memory.size_bytes` / `memory.ballooning` / `memory.zram` / `memory.ksm` / `memory.mem_lock` / `memory.hugepages` | size string / `true`\|`false` | Instance must be stopped |
| `disk.thin_provisioning` / `disk.trim_on_shutdown` / `disk.compact_on_shutdown` / `disk.snapshot_timeout_secs` | boolean / seconds | Instance must be stopped |
| `display.dpi` / `display.fps_limit` / `display.display_engine` / `display.fullscreen` | int / engine name / boolean | Instance must be stopped |
| `gpu.render_backend` / `gpu.hostmem_bytes` / `gpu.blob` / `gpu.gl` | backend name / bytes / boolean | Instance must be stopped |
| `network.mode` / `network.device_model` / `network.nat_backend` | mode / model / backend name | Instance must be stopped |
| `network.port_forwards` | read-only | Immutable: fixed at create time (QEMU netdev `hostfwd=`) |
| `kind.android_profile.boot_mode` / `kind.android_profile.arm_translator` / `kind.android_profile.android_version` | read-only | Immutable: use the dedicated `guest boot-mode` / `guest install <translator>` commands |
| `audio.backend` / `audio.device` | backend / device name | Instance must be stopped |
| `input.pointer_mode` / `input.hide_host_cursor` / `input.clipboard_enabled` | mode / boolean | Instance must be stopped |
| `firmware.enable_uefi` | `true`\|`false` | Instance must be stopped |

Anything else is rejected with `invalid config key: <key>` (`INVALID_ARGUMENT`), naming the reason (immutable or unknown key).

The flag form `andler config --instance <id>` is equivalent to `view`; add `--edit`/`-e` for the editor. Protected fields (`id`, `kind`, `disk.path`) cannot be changed.

### `remove`

```bash
andler remove <instance-id> [--purge]
```

Without `--purge`: removes the instance record and its registry files (`instance.toml`, `events.jsonl`) and marks the registry directory with `instance.removed`, so a daemon restart no longer scans it (without the marker, a saved directory missing its toml would resurface as a broken entry). Disk and OVMF vars remain on disk — including inside the registry directory.
With `--purge`: also deletes `disk.path` and `firmware.ovmf_vars_path`. Never deletes `base_image` or `ovmf_code_path` (shared across instances).

Instance must be in a terminal state (`Created`, `Stopped`, or `Error`). Use `stop` first for running instances.

With `--json`, remove prints `{ "instance_id" }`.

With `--purge`, refuses if the instance has live `Linked` clones (deleting the source disk would break them).

With `--purge` on an interactive terminal, asks for confirmation before deleting; answering `n` prints `Cancelled.` and leaves the instance untouched. Non-interactive (scripted) runs skip the prompt.

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

With `--json`, clone prints `{"instance_id", "source_instance_id"}`.

### `export`

```bash
andler export <source-id> <dest-path>
```

Exports the instance disk as a standalone file at `dest_path`. Does not create a new instance. Source must be in a terminal state (`Created`, `Stopped`, or `Error`).

With `--json`, export prints `{"dest_path", "source_instance_id"}`.

### `export-oci`

```bash
andler export-oci <source-id> <dest-path> --disk-format <qcow2|raw|vdi> [--disk-path <PATH>]
```

Exports the instance disk as an OCI image layout directory at `dest_path` (an `oci-layout` marker, `index.json`, `config.json`, and a rootfs layer blob under `blobs/sha256/`). Does not create a new instance. Source must be in a terminal state (`Created`, `Stopped`, or `Error`). The source disk must be qcow2; `--disk-path` overrides the source disk path.

With `--json`, export-oci prints `{"dest_path", "source_instance_id"}`.

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

### Daemon logs

```bash
andler logs daemon [--follow] [--json] [--since <epoch-ms>]
```

Streams the daemon's own log (the same lines it prints to stdout/stderr,
in the same format — JSON when `ANDLERD_LOG_FORMAT=json`), served from an
in-memory ring of the last 4096 lines, so debugging never requires knowing
where the daemon writes. The special id `daemon` is reserved for this
command and is never a valid instance id.

| Flag | Description |
|------|-------------|
| `--follow` | Keep streaming new lines instead of returning the ring snapshot |
| `--json` | JSON-lines output: `{"ts_ms":...,"line":...}` |
| `--since <epoch-ms>` | Only lines at/after this timestamp (snapshot mode) |

`--follow/--json/--since` are rejected when an instance id is given; the
instance form has its own flags (`--source`, `--grep`, `--tail`).

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
# Create (live; requires Running/Paused instance and a qcow2 disk)
andler snapshot create <instance-id> --tag before-update --description "Pre-upgrade state"

# Restore (offline; requires a stopped instance — fails if Running/Paused)
andler snapshot restore <instance-id> --tag before-update
# Branch restore: keep the current chain as an archived branch
andler snapshot restore <instance-id> --tag before-update --branch

# Delete (offline; requires a stopped instance — fails if Running/Paused)
andler snapshot delete <instance-id> --tag before-update

# List (any state)
andler snapshot list <instance-id>
andler snapshot --json list <instance-id>
```

Snapshots are disk-only **external** qcow2 snapshots: every create (live, over QMP) turns
the current `disk.qcow2` into an overlay layer under `disk.snapshots/<uuid>.qcow2` and
starts a fresh overlay as the new active disk — the guest keeps running and the previous
state stays addressable by tag. Restore (offline, `qemu-img`-based) rebuilds the active
disk on top of the target layer and takes effect on the next start — the guest reboots,
RAM is not restored. Restore **discards** layers newer than the target unless
`--branch` is passed, which archives the current chain (tagged `pre-branch-<ts>`,
`branch-<ts>` in the list) and continues from the target; restoring `--branch` onto a
snapshot of an archived branch switches back to that branch. Non-qcow2 disks cannot be
snapshotted (`external snapshots need overlay support — convert the disk to qcow2
first`); legacy internal snapshots can be listed/deleted but not restored
(`--branch`-independent). `--timeout` is accepted for CLI compatibility but unused (the
operations are synchronous).

Restore/delete are refused while linked clones derive from the instance's disk chain
(`remove the clones first`), and deleting a layer is refused while another instance's
chain contains it. Deleting the base layer (the first snapshot) is refused — there is no
parent to merge its data into.

`snapshot restore` accepts `--idempotency-token <key>` (online restore only).
A repeat with the same token while a restore is in-flight joins the running
restore; a repeat with a *different* token is refused with
`OperationAlreadyRunning`. Omit the flag and the operation's own id is used as
the key.
A cancelled restore is never joined: after `op cancel`, a same-key repeat
starts a fresh restore.

`delete` asks for confirmation on an interactive terminal (answering `n` prints `Cancelled.` and keeps the snapshot).

List output shows `tag`, `id`, `created_at` (human-readable local time), `description`,
and `branch` (only for archived branch snapshots). `--json` (placed before the subcommand)
emits a JSON array of `{ "tag", "snapshot_id", "created_at", "description", "branch" }`.

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

**Size format**: Supports `GB`, `GiB`, `MB`, `MiB`, `TB`, `TiB` (case-insensitive). Space between number and unit is optional. Plain number = bytes. Sizes must be greater than zero (a 0-byte disk is rejected with `disk: refusing to create a 0-byte disk image`).

| Command | Description |
|---------|-------------|
| `create <path> --size <size>` | Create a new empty qcow2 disk (`--size` required) |
| `info <path>` | Show disk info (virtual size, actual usage, format, backing file) |
| `resize <path> --size <size>` | Resize an existing disk (requires `--shrink` to reduce size) |
| `compact <path>` | Compact a disk (reclaim unused space via `qemu-img convert`, only works on qcow2) |

The flag form (`andler disk --create --path <p> --size <s>`, `--info`, `--resize`, `--compact`) is also accepted; the action flags are mutually exclusive (`disk: actions are mutually exclusive, got --create and --info`), and at least one is required.

`disk info --json` prints `{"path", "format", "virtual_size", "actual_size", "backing_file"}` (`backing_file` is `null` when the disk has no backing file).

### `op`

Long-running operation inspection and cancellation. Snapshot restores and
other multi-phase operations run as tracked operations (one per instance);
`op list` shows the running phase, `op cancel` asks the operation to stop at
its next cancel point.

```bash
# List active operations (human-readable or --json)
andler op list
andler op list --json

# Cancel a running operation (operation stops at its next cancel point)
andler op cancel <op-id>
```

| Command | Description |
|---------|-------------|
| `list [--json]` | List active operations: op id, instance, kind, current phase, progress, state |
| `cancel <op-id>` | Cancel a running operation; unknown id is an error |

### `connect`

Single entry point to the guest. `--level` selects the access method;
`auto` picks by effective profile — Android VMs booted into `linux` mode go
to ssh, everything else to the serial console.

| Level | Mechanism | Requires |
|-------|-----------|----------|
| `console` | Direct attach to the VM's serial console (raw terminal; works headless, no guest OS needed) | Instance running |
| `ssh` | Spawns `ssh -p <host_port> user@localhost` using the configured `network.port_forwards` entry for guest port 22 | Running guest with sshd; `network.port_forwards` set at create |
| `adb` | Spawns `adb connect localhost:<host_port>` using the forward for guest port 5555 | Running Android guest with adb; forward configured |
| `auto` | `console` for Linux VMs and Android-in-android mode; `ssh` for Android VMs booted into linux mode | — |

```bash
# Attach to the serial console (raw mode; Ctrl-C detaches)
andler connect <instance-id> --level console

# Interactive ssh session to the forwarded guest port
andler connect <instance-id> --level ssh

# Point adb at the forwarded guest port
andler connect <instance-id> --level adb
```

The console socket is a chardev under `$XDG_RUNTIME_DIR/andler/console/<id>.sock`
(`server=on,wait=off`); the serial log still lands in `console.log` next to
the instance disk, so attach sessions never lose the historical log.

### `events`

Streams daemon events from the live bus — lifecycle transitions,
operation state changes, raw QMP events and diagnostic log lines.
Exits after the first event unless `--follow` keeps streaming, which
makes one-shot invocation scriptable.

```bash
# One event (e.g. the instance-created line right after a create)
andler events

# Stream events for one instance as JSON lines
andler events <instance-id> --json --follow

# One lifecycle event for a specific instance (prefix resolution works)
andler events <instance-id>
```

| Flag | Description |
|------|-------------|
| `<instance-id>` | Optional filter (full id or unique prefix); all instances when omitted |
| `--follow` | Keep streaming instead of exiting after the first event |
| `--json` | JSON-lines output (`ts_ms`, `instance_id`, `kind`, `detail`) |

### `exec`

Runs a command inside the guest through the guest agent and relays its exit
code — a programmatic access level that needs no guest network setup.

```bash
andler exec <instance-id> -- uname -a
andler exec <instance-id> -- sh -c 'echo hi > /tmp/marker'
```

Requires the instance Running/Paused with a responsive `qemu-guest-agent`;
stdout/stderr are relayed and the CLI exits with the guest's exit code.
With `--json`, prints `{ "exit_code", "stdout", "stderr" }`; the CLI still exits with the guest's exit code.

### `guest`

Guest package management — install, remove, or list packages in the guest OS. Auto-fallback: if VM is running and guest agent is available → online via the guest agent socket (`guest-exec`); if VM is stopped → offline via `guestmount` (FUSE) + an unprivileged user namespace — zero root either way.

Mode selection by instance state:

| State | Behavior |
|-------|----------|
| `Running` | Online via the guest agent. If `qemu-guest-agent` is not installed/responding, the command fails with a hint to stop the VM first (offline path) — no silent fallback. |
| `Paused` | Treated like online, but the frozen guest agent cannot respond, so the command fails with a hint to resume or stop the VM. |
| `Created` / `Stopped` / `Error` | Offline via `guestmount` (FUSE) + `unshare` user namespace + chroot — zero root; requires `libguestfs-tools`, `/dev/fuse`, and unprivileged user namespaces (see `andler doctor`). |
| `Starting` / `Stopping` | Rejected. |

```bash
# Install a package (smart path: online via guest agent when running;
# auto-starts a stopped VM for maintenance and stops it again; --offline
# forces the offline guestmount/userns path for VMs that cannot boot)
andler guest install spice-vdagent <instance-id>
andler guest install spice-vdagent <instance-id> --offline

# Remove a package
andler guest remove spice-vdagent <instance-id>

# List known packages and their status
andler guest list <instance-id>

# Apply a provision manifest (online via QGA when running, offline via the
# guestfs appliance when stopped)
andler guest provision ./manifest.toml <instance-id>

# Switch Android boot mode (Android instances)
andler guest boot-mode <instance-id> [android|linux]
```

`guest install/remove` without `--offline` never needs root on the host:
a stopped instance is booted headless, the package is installed via the
guest agent, and the VM is stopped again; on an already-running VM the
operation runs in place. Both forms are supervisor operations — progress
is visible on `andler events` and cancellable. If the guest agent does not
appear within `ANDLERD_GUEST_AGENT_WAIT_SECS` (default 120 s) the
operation fails with a hint to retry with `--offline` — that path uses
the offline guestmount + userns path — zero root, no sudoers rules.
Both `guest install` and `guest remove` accept `--idempotency-token <key>`, a
client-chosen key that makes a network retry idempotent: a second call with the
same token while the first is still in-flight joins the running operation
(install/remove runs once, and the second call returns its result) instead of
starting a duplicate. A call with a *different* token while one is in-flight is
refused with `OperationAlreadyRunning` (a `FAILED_PRECONDITION`). Omit the flag
and the operation's own id (`guest-install`/`guest-remove` + package) is used as
the key, so a repeat of an identical call still joins. The token is only used on
the online path (a running VM or a VM auto-started for maintenance); on the
offline `--offline` path it is accepted but unused.
A cancelled operation is never joined: after `op cancel`, a same-key repeat
(token or no token) starts a fresh install/remove instead of joining the
cancelled operation.

`guest list --json` prints `{"packages": [{"name", "description", "status"}]}`.

### Provision manifests

`guest provision <manifest> <instance-id>` applies a declarative TOML
manifest (canonical copies live in `docker/images/guest-components/`).
Running instance → applied online through the guest agent (zero root);
stopped instance → applied offline through the guestfs appliance (zero
root, one appliance session per call). Paused is rejected. All ops map
1:1 onto the shared `MutatorOp` batch — write, upload, mkdir, cp, mv,
rm-rf, chmod, symlink:

```toml
schema_version = 1
name = "spice-guest"

[[ops]]
op = "mkdir-p"
path = "/usr/local/bin"

[[ops]]
op = "upload-file"          # host_path is resolved relative to the manifest dir
guest_path = "/usr/local/bin/spice-agent"
host_path = "./bin/spice-agent"
mode = "0755"               # optional; becomes a trailing chmod op

[[ops]]
op = "write-file"
path = "/etc/spice.conf"
content = "enabled = true\n"
mode = "0644"

[[ops]]
op = "symlink"
target = "/usr/local/bin/spice-agent"
link = "/usr/bin/spice-agent"
```

`mode` is an octal string (1–4 digits); any invalid mode or an unknown
`schema_version` / `op` kind / field fails the whole call before anything
is applied. An empty op list is refused.

### Base-image pin

Every Android instance records the image it was created from in its
`instance.toml` (`kind.android_profile.base_image_pin = { id, sha256 }`):
the manifest id (`android{version}-{variant}-{built_at}`) plus the
content sha256 of the qcow2. The pin is written at creation time — when
the instance file already carries one, creation refuses an image whose
id or checksum no longer matches, instead of silently building on a
swapped backing file (which would corrupt every linked clone on top of
it). Explicit updates are manual by design: remove or edit
`base_image_pin` in the instance file to accept a different image. The
check runs at creation only — an existing instance disk references its
backing file directly, and re-hashing a multi-GB image on every start
would cost seconds per boot for no protection.

Known packages: `spice-vdagent` (shared folders), `qemu-guest-agent` (host-guest communication), `spice-webdavd` (webdav shared folders).

**ARM translators**: `install libndk <id>` / `install libhoudini <id>` are special-cased — they go through `SwitchArmTranslator` (offline disk staging) instead of the package-manager path. There is **no online path**: the translator is written into the stopped VM's disk overlay, so `Running`/`Paused` instances are rejected with "must be stopped … stop it first". Works before the guest's first boot — the `var/lib/waydroid/overlay` upper dir is created on the disk if `waydroid init` hasn't run yet. Optional `--translator-dir <path>` points at a local cache directory with the extracted translator instead of downloading it. Without it, the daemon downloads the translator zip (~18 MiB) from GitHub on first use, caches it in `~/.andler/cache/arm-translators/`, and verifies its MD5; the download has a 15 s connect timeout and a 5 min total timeout — a broken/slow connection fails with a clear error pointing at `--translator-dir` instead of hanging forever. Daemon logs (`andler logs` / `RUST_LOG=info`) report each stage (download → md5 → extract).

| Command | Description |
|---------|-------------|
| `install <package> <instance-id>` | Install a package in the guest OS |
| `remove <package> <instance-id>` | Remove a package from the guest OS |
| `list <instance-id>` | List known packages and their status (installed/not installed) |
| `provision <manifest> <instance-id>` | Apply a provision manifest (see above) |
| `boot-mode <instance-id> [mode]` | Get or set the Android boot mode (`android`/`linux`) |

### `attach`

Hot-plugs an extra disk or network device into a **running or paused** instance. The device is recorded in `extra_disks`/`extra_networks` in `instance.toml` and re-created automatically on the next `start` — no config step needed after a reboot.

```bash
# Attach a new 20 GiB disk (creates the qcow2 image)
andler attach disk <instance-id> --path /data/games.qcow2 --size 20G

# Attach an existing image (its actual virtual size is used; --size is ignored)
andler attach disk <instance-id> --path /data/backup.qcow2

# No --path: creates disk-extraN.qcow2 inside the instance directory (--size required)
andler attach disk <instance-id> --size 10G

# Attach a NAT network device (default model virtio-net-pci)
andler attach net <instance-id>

# Attach a bridged network device on a host bridge
andler attach net <instance-id> --mode bridge --bridge br0

# NAT backend selection (slirp is the default)
andler attach net <instance-id> --nat-backend passt
```

| Flag | Applies to | Description |
|------|------------|-------------|
| `--path <path>` | disk | Image path; absolute paths are used as-is, plain file names land in the instance directory, omitted → `disk-extraN.qcow2` in the instance directory |
| `--size <size>` | disk | Virtual size for a new image (same format as `disk create`); required when the path does not exist yet; `0` is rejected |
| `--mode <nat\|bridge\|isolated>` | net | Network mode (default `nat`) |
| `--bridge <iface>` | net | Host bridge for `--mode bridge` |
| `--model <model>` | net | Device model (default `virtio-net-pci`) |
| `--nat-backend <slirp\|passt>` | net | NAT implementation (default `slirp`) |

With `--json`, prints `{ "action": "attach_disk", "instance_id", "path", "index" }` (attach disk), `{ "action": "attach_network", "instance_id", "index" }` (attach net), `{ "action": "detach_disk", "instance_id", "path" }` (detach disk), or `{ "action": "detach_network", "instance_id", "index" }` (detach net).
Prints the resolved disk path + index, or the network index (0-based, in attach order). Attaching an already-attached path (including the primary disk) fails with `already attached`. Extra disks are **not** covered by snapshots (`snapshot create` snapshots the primary `drive-disk0` only).

With `--json`, detach prints the same `{ "action", "instance_id", "path"/"index" }` object as attach.

### `detach`

Hot-unplugs a previously attached device. The disk image file is **never deleted** — only the config entry and the live QEMU device.

```bash
# Detach by path (as shown by `andler config <id>`)
andler detach disk <instance-id> /data/games.qcow2

# Detach by index (0-based, in attach order)
andler detach net <instance-id> 0
```

Detaching a path/index that is not attached fails with `is not attached` and points at `andler config <id>` for the current lists.

### `wizard`

```bash
andler wizard
```

Interactive guided instance creation wizard (also the default when `andler` is invoked with no subcommand). Walks through all configuration options with smart defaults and hardware auto-detection. Prints a full summary before creation and offers `Create VM` / `Modify advanced settings` / `Cancel`.

### `doctor`

```bash
andler doctor [--metrics]
```

Checks the local environment for ANDLER prerequisites: KVM availability, QEMU/OVMF installation, daemon reachability, base images, and the offline-guest-operation prerequisites (`guestmount`/libguestfs on PATH, `/dev/fuse`, unprivileged user namespaces allowed — the `sysctl kernel.unprivileged_userns_clone=1` hint appears on Debian/Ubuntu when disabled). Read-only — works even if andlerd isn't running. Offline guest package ops (`--offline`) are zero-root: no sudoers rules, nothing to install.

With `--json`, prints `{ "overall": "ok"|"needs_attention", "checks": [ ... ] }` — the same `overall`/`checks` document the human-readable report is derived from.

With `--metrics`, prints the daemon's internal metrics snapshot instead of the environment checks: RPC latency p50/p99 by method, error counts by gRPC status code, instance counts, active operations, QMP reconnects, and running guest RAM (the running-total the memory-overcommit gate sums before spawn; only shown when non-zero). (requires a reachable daemon).
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
overlay_size_gib = 128
gapps = false
microg = false
arm_translator = "none"   # or "libndk" / "libhoudini"
instances_root = "/home/user/.andler/instances"
```

### Linux VM (full example)

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.andler/my-linux-vm/VARS.fd"

disk_size_gib = 100
snapshot_timeout_secs = 60   # accepted for compatibility; snapshot ops are synchronous
autostart = true             # start this instance automatically when the daemon starts 

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
mem_lock = false
hugepages = false

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
[[network.port_forwards]]
protocol = "Tcp"
host_port = 2222
guest_port = 22

[firmware]
ovmf_code_path = "/usr/share/edk2/x64/OVMF_CODE.4m.fd"
ovmf_vars_path = "/path/to/VARS.fd"

[audio]
backend = "Pipewire"
device = "VirtioSound"

[input]
pointer_mode = "Tablet"
hide_host_cursor = true
clipboard_enabled = true
```

Hotplugged devices are persisted as array-of-tables (added by `andler attach`, editable by hand when the instance is stopped):

```toml
[[extra_disks]]
path = "/data/games.qcow2"
size_bytes = 21474836480
format = "Qcow2"
thin_provisioning = true
trim_on_shutdown = false
compact_on_shutdown = false

[[extra_networks]]
mode = "Nat"
device_model = "virtio-net-pci"
nat_backend = "Slirp"
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
| `firmware` | OVMF_CODE at `/usr/share/edk2/x64/OVMF_CODE.4m.fd` |
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
| `ListInstances` | `Empty` | `ListInstancesResponse` | Unary |
| `RemoveInstance` | `RemoveInstanceRequest` | `Empty` | Unary |
| `GetInstanceConfig` | `InstanceIdRequest` | `GetInstanceConfigResponse` | Unary |
| `UpdateInstanceConfig` | `UpdateInstanceConfigRequest` | `Empty` | Unary |
| `StreamInstanceLogs` | `InstanceIdRequest` | `stream LogLineResponse` | Server-streaming |
| `StreamResourceMetrics` | `InstanceIdRequest` | `stream ResourceMetricsResponse` | Server-streaming |
| `CloneInstance` | `CloneInstanceRequest` | `CreateInstanceResponse` | Unary |
| `ExportInstanceDisk` | `ExportInstanceDiskRequest` | `ExportInstanceDiskResponse` | Unary |
| `ExportInstanceOci` | `ExportInstanceOciRequest` | `ExportInstanceOciResponse` | Unary |
| `CreateSnapshot` | `CreateSnapshotRequest` | `CreateSnapshotResponse` | Unary |
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary |
| `ListSnapshots` | `InstanceIdRequest` | `ListSnapshotsResponse` | Unary |
| `InstallGuestAgent` | `InstallGuestAgentRequest` | `Empty` | Unary |
| `RemoveGuestAgent` | `RemoveGuestAgentRequest` | `Empty` | Unary |
| `ListGuestPackages` | `InstanceIdRequest` | `ListGuestPackagesResponse` | Unary |
| `SwitchArmTranslator` | `SwitchArmTranslatorRequest` | `Empty` | Unary |
| `SetInstanceConfig` | `SetInstanceConfigRequest` | `Empty` | Unary |
| `SwitchAndroidBootMode` | `SwitchAndroidBootModeRequest` | `Empty` | Unary |
| `GetAndroidBootMode` | `InstanceIdRequest` | `GetAndroidBootModeResponse` | Unary |
| `AttachDisk` | `AttachDiskRequest` | `AttachDiskResponse` | Unary |
| `DetachDisk` | `DetachDiskRequest` | `Empty` | Unary |
| `AttachNetwork` | `AttachNetworkRequest` | `AttachNetworkResponse` | Unary |
| `DetachNetwork` | `DetachNetworkRequest` | `Empty` | Unary |

### Error Codes

| gRPC Status | Daemon Error | When |
|-------------|--------------|------|
| `NOT_FOUND` | `InstanceNotFound`, `SnapshotNotFound`, `InstanceRefNotFound`, `DiskNotAttached`, `NetworkNotAttached` | Unknown instance/snapshot/ref, or detaching a device that is not attached |
| `UNIMPLEMENTED` | `NoBackendRegistered`, `Backend(NotImplemented)` | Backend kind not available |
| `FAILED_PRECONDITION` | `InvalidTransition`, `InstanceNotRemovable`, `InstanceNotClonable`, `InstanceAlreadyStopped`, `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`, `SnapshotOperationRequiresRunningInstance`, `SnapshotLimitExceeded`, `GuestAgentUnavailable`, `NotAndroid`, `InstanceMustBeStopped`, `HotplugRequiresRunningInstance`, `MemoryOvercommit`, `Backend(HandleNotFound)`, `Backend(ProcessNotRunning)` | Wrong lifecycle state, resource limit, guest agent unavailable, wrong instance kind |
| `ALREADY_EXISTS` | `SnapshotAlreadyExists`, `DiskAlreadyAttached` | Duplicate snapshot tag, or disk image already attached |
| `INVALID_ARGUMENT` | `ConvertError`, `EmptyInstanceRef`, `MalformedInstanceRef`, `AmbiguousInstanceId`, `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged`, `InvalidConfig`, `InvalidConfigKey`, `MissingOvmfVarsTemplate` | Malformed request or invalid arguments |
| `RESOURCE_EXHAUSTED` | `InsufficientDiskSpace` | Not enough free space for a snapshot operation |
| `INTERNAL` | Other `Backend`/`Disk`/`Io`/`Store`/`Firmware` errors — including all `andler-disk` errors other than `InsufficientDiskSpace` (`AgentNotInstalled`, `AgentAlreadyInstalled`, `PackageManagerNotFound`, ...) | Backend/disk/store failures |

All error messages pass through `status_message()`: control characters (other than tab) are replaced with spaces and messages longer than 384 characters are truncated before being sent as the gRPC `grpc-message` header (long multi-line package-manager stderr otherwise trips the h2 client with "h2 protocol error").

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
  string instance_ref = 1;
  string name = 2;
  InstanceKind kind = 3;
  BackendKind backend = 4;
  CpuConfig cpu = 5;
  MemoryConfig memory = 6;
  DiskConfig disk = 7;
  DisplayConfig display = 8;
  GpuConfig gpu = 9;
  NetworkConfig network = 10;
  FirmwareConfig firmware = 11;
  AudioConfig audio = 12;
  InputConfig input = 13;
}
```

`UpdateInstanceConfig` requires the instance to be stopped (disk idle) — a running instance gets `InstanceMustBeStopped` (`FAILED_PRECONDITION`). Protected fields (`id`, `kind`, `disk.path`) cannot be changed: `id` is always taken from the request's `instance_ref` (`ConfigIdMismatch` if a different id is supplied), `kind` changes are `ConfigKindChanged`, `disk.path` changes are `ConfigDiskPathChanged` — all `INVALID_ARGUMENT`. `cdrom_bus` (`kind.linux_vm.cdrom_bus`) is validated the same way as on create: `UNSPECIFIED` is rejected with `ConvertError::MissingField`.
