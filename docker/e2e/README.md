# docker/e2e — building and testing ANDLER in containers

Containerized build + test harness: reproducible builds (same Rust/system
library versions everywhere), unit/integration tests, and a full end-to-end
suite that drives the real daemon, CLI, QEMU/KVM, and SQLite.

## Files

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage: `builder` → `testbuild` → `unit-test` / `integration-test` / `e2e` / `daemon`. Details in the file itself. |
| `compose.yaml` | Compose shortcuts; every service builds with context `../..` (repository root). |
| `e2e.sh` | Suite orchestrator: fresh daemon, runs each `tests/NN_*.sh` suite, prints a per-suite summary. |
| `tests/common.sh` | Assertion helpers (`expect_ok`, `expect_fail`, `expect_out_grep`, …), daemon lifecycle, fixture builders. |
| `tests/NN_*.sh` | One self-contained suite per functional area (lifecycle, config, disk, snapshots, clone/export, guest, preview/verify, persistence, ops, connect, exec, port forwards, boot mode, version, events). |

All commands below run from the **repository root**.

## Quick start

```bash
# Unit tests for the whole workspace — no /dev/kvm needed, works anywhere.
docker compose -f docker/e2e/compose.yaml build unit-test
docker compose -f docker/e2e/compose.yaml run --rm unit-test

# Integration tests (#[ignore] tests: qemu-img, QMP round-trip, spawn).
# Requires /dev/kvm.
docker compose -f docker/e2e/compose.yaml build integration-test
docker compose -f docker/e2e/compose.yaml run --rm integration-test

# Full E2E suite: real andlerd + andler over gRPC, QEMU under KVM, sqlite.
# Requires /dev/kvm.
docker compose -f docker/e2e/compose.yaml build e2e
docker compose -f docker/e2e/compose.yaml run --rm e2e
```

### Iteration speed

BuildKit cache mounts (registry + target) make source-only rebuilds
incremental: after changing Rust code, `docker compose build e2e` recompiles
only the affected crates. No `--no-cache` needed — that flag forces a full
rebuild and should only be used after dependency changes.

The `unit-test`/`integration-test` images bake a full cargo cache at build
time, so `docker compose run` executes tests without rebuilding and without
network access.

If a toolchain bump (or a corrupted cache) produces stale artifacts:

```bash
docker builder prune --filter type=exec.cachemount
```

## What runs in each target

| Target | What it does | Needs `/dev/kvm` | Needs `qemu-img` |
|--------|-------------|:-----------------:|:-----------------:|
| `unit-test` | `cargo test --workspace` — all tests without `#[ignore]` | No | No |
| `integration-test` | `cargo test --workspace -- --ignored` — `#[ignore]` tests (qemu-img, QMP, spawn) | Yes | Yes |
| `e2e` | `e2e.sh` — the full E2E suite (see below) | Yes | Yes |
| `daemon` | Minimal runtime image for `andlerd` (manual experiments only) | Yes | Yes |

## The E2E suite

`e2e.sh` starts a fresh `andlerd` on an isolated sqlite store and runs every
`tests/NN_*.sh` suite in order. Each suite is self-contained (creates and
removes its own instances) and prints an assertion count; the orchestrator
prints a summary and exits non-zero when any suite fails. Each suite gets a
fresh daemon and store, and `stop_daemon` waits for the old process to exit
before the next `start_daemon`: a `kill` returns long before the process is
gone, and the replacement used to lose the flock/listen race and exit while the
readiness probe was still answered by the dying daemon — every command after
that then talked to a daemon started with a different environment. Each suite runs under a wall-clock watchdog (`E2E_SUITE_TIMEOUT`, default 600s): a hung suite is killed, its remaining processes and daemon.log tail are dumped for diagnosis, and the next suite still starts on a clean daemon. PASS/FAIL lines carry the suite's elapsed time.

Coverage by suite:

| Suite | Covers |
|---|---|
| `01_lifecycle.sh` | create (`--file`, flag negatives), list/status (text + `--json`, short-id resolution, not-found), start/pause/resume/stop with FSM negatives, metrics (`--once`, `--json`), log streaming (SIGTERM line, empty after stop), remove with/without `--purge` + file semantics |
| `02_config.sh` | `config view`, `config set` (name, `display.resolution`, malformed value, unknown key), `config edit` via `$VISUAL` (success + broken TOML), `config status` (in sync, hand-edit applied on idle instance) |
| `03_disk.sh` | disk create/info/resize (grow + shrink refusal)/compact (qcow2 + raw), zero-size and unparsable-size negatives |
| `04_snapshot.sh` | external overlay snapshots: live create (incl. duplicate tag)/list/`--json` + layer files on disk, offline discard restore (newer layer removed), offline delete of a middle layer (commit + prune, active re-pointed at the parent) and base-layer delete refusal, restore/delete-while-running, create-while-stopped, `--branch` restore (archived `pre-branch-*` head, `branch=*` in list, switch-back, all layers kept), remove-while-running |
| `05_clone_export.sh` | Android create (incl. missing base image), clone linked/full-standalone/shared-base, live-clone removal protection, export (file + no new instance), nonexistent-source negatives |
| `06_hotplug.sh` | Hotplug on a live VM: `attach`/`detach` of extra disks and network devices, config persistence across the operation, host-side rollback when the guest refuses (`device_del` timeout), and the state-machine negatives (`FAILED_PRECONDITION` while stopped) |
| `07_guest.sh` | Guest error paths always; deep tests (privileged container): offline `guest list`/`install`/`remove` with the explicit `--offline` flag against a real Debian rootfs (zero-root: guestmount + user-namespace chroot), the smart maintenance auto-start path (stopped VM without QGA fails with an `--offline` hint and ends up stopped again; wait capped via `ANDLERD_GUEST_AGENT_WAIT_SECS`), a provision manifest applied offline via the guestfs appliance with the result verified through an nbd mount (content + octal modes + symlink), Android boot-mode switching, ARM translator install, and `guest apply` (the instance's own config — ARM translator, clipboard agent — applied per selection with a classified outcome) through the guestfish appliance (Debian kernel shipped in the image; `/lib/modules` is deliberately not bind-mounted so the appliance boots its own modules) |
| `08_preview_verify.sh` | `create --dry-run` (nothing created), `--verify` pass/fail, wizard non-TTY refusal, shell completions, `andler doctor` sections (and that no `sudoers`/`andler-helper` checks remain) |
| `09_persistence.sh` | daemon restart against the same store (state survives, incl. a never-started instance), registry audit log (`events.jsonl` next to `instance.toml`), broken registry entries (`[broken: …]` in list, removable), single-daemon flock (`andlerd.lock` refuses a second daemon), final cleanup |
| `10_base_images.sh` | Android base-image auto-discovery (`cache/base-images/<major>-<variant>/` subdir layout + legacy flat-root fallback), base-image pin recorded on create and enforced (checksum tamper and swapped id both refused), `cache list` (all builds, human + `--json`) and `cache clean` (superseded-build + orphan-manifest/orphan-qcow2 removal, dry-run then real, idempotent on an up-to-date cache) |

| `11_process_supervision.sh` | `kill -9` of the live qemu process → instance lands in `Error { QEMU process exited unexpectedly }` promptly via pidfd death notification, restart-from-Error recovery, clean stop/remove |
| `12_process_reconnect.sh` | SIGKILL of the daemon while the instance is Running → qemu survives, restarted daemon adopts it (`Running`, not `Error`), stop terminates the adopted process, remove leaves no strays |
| `13_chain_reconcile.sh` | chain reconciliation across daemon restarts: clone protection (restore/delete refused while a linked clone consumes the chain, `remove the clones first`), crash-mid-restore rebuild of `disk.qcow2` on the chain head, crash-between-rename-pair promotion of the staging overlay (plus its `recovered-<uuid8>` entry), orphaned `.tmp-*` removal, crash-after-rename-pair layer recovery, metadata-entry-without-layer-file drop (archived branch head deleted behind the daemon's back), `--branch` switch-back returning ancestors to the main branch, final cleanup |
| `14_op_progress.sh` | snapshot restore runs as a tracked operation: operation events in `events.jsonl` (kind/state/phase), `op list`/`op list --json`, `op cancel` of an unknown id, final cleanup |
| `15_connect.sh` | `andler connect --level console`: stopped-instance negative (`start it first`), live attach relays OVMF serial output (BdsDxe), unknown-instance rejection, `console.log` tee next to the disk |
| `16_exec.sh` | `andler exec`: stopped/unknown negatives (`requires instance … to be running`), running-without-guest-agent negative (`guest agent is not available`) |
| `17_port_forwards.sh` | `network.port_forwards` round-trip: `create --file` with the key, `--dry-run` cmdline contains `hostfwd=tcp::2222-:22`, live TCP connect to the forwarded port while the VM runs, `connect --level ssh` degraded branch |
| `18_boot_mode.sh` | P31: `config view` surfaces `boot_mode: android` from the config (no disk mount), `config set` refuses the key (immutable), `guest boot-mode` get is config-backed |
| `19_version.sh` | version handshake: a matching CLI/daemon pair passes through |
| `20_events.sh` | `andler events`: lifecycle log event on create, instance-filtered stream during start/stop (JSON), cleanup |
| `21_daemon_logs.sh` | `andler logs daemon`: startup line in the snapshot ring, future `--since` yields nothing, `--follow` stays alive until timeout, `--json` shape |
| `22_daemon_metrics.sh` | `andler doctor --metrics`: snapshot header/counts, recorded RPC traffic after real commands (GetVersion/ListInstances visible) |
| `23_online_install.sh` | The default online package path against a genuinely booted guest: the official Debian genericcloud image (baked at build time) + NoCloud seed — cloud-init brings up DHCP, installs and enables qemu-guest-agent (DNS workaround: pinned slirp resolver, hardcoded mirror IPv4s, ForceIPv4); then `guest install/remove` runs online via QGA, verified with guest-exec; the same `--idempotency-token` flag round-trips through the CLI/proto/service/daemon and is exercised live — a repeat install with the same token returns success (joining the in-flight online op) |
| `25_disk_conflict.sh` | Two instances pointing at the same disk file: the second start is refused up front with an actionable `DiskInUse` error instead of QEMU's opaque write-lock failure, while a shared read-only base image stays legal |
| `24_autostart.sh` | Autostart: `create --file` with `autostart = true` reaches Running after a daemon restart with exactly one QEMU process (no double start), a non-marked instance stays Stopped, `config set autostart false/true` toggles the flag and survives a second restart |
| `26_affinity.sh` | CPU pinning: `cpu.affinity = [0, 1]` in `instance.toml` pins the QEMU process (verified via `/proc/<pid>/status` `Cpus_allowed_list`), an overlapping pin on a running instance is refused up front, a disjoint pin starts fine, an index beyond the host CPU count is rejected before spawn |
| `27_mem_lock.sh` | `memory.mem_lock` config key: false by default, `config set` true/false round-trips through `config view`, malformed value rejected, survives a daemon restart. The QEMU arg it maps to (`-overcommit mem-lock=on`) is asserted by the cmdline unit test |
| `28_hugepages.sh` | `memory.hugepages` config key: false by default, `config set` true/false round-trips through `config view`, malformed value rejected, survives a daemon restart. The QEMU arg it maps to (`memory-backend-file` with `mem-path=/dev/hugepages`) is asserted by the cmdline unit test |
| `29_overcommit_gate.sh` | Memory overcommit gate: starting an instance whose guest RAM plus the RAM already used by running instances exceeds host physical RAM fails with a `MemoryOvercommit` FailedPrecondition error (`would exceed host memory`) rather than a raw QEMU spawn failure. Host RAM is read from `/proc/meminfo`; the gate's arithmetic is pinned by the daemon unit test
| `30_json_output.sh` | `--json` output contract: a successful command emits a JSON document to stdout, and an error path emits `{"error": ...}` to stderr (never a bare text line) — pins the unified JSON error refactor |
| `32_image_download.sh` | Base-image downloads against a local fixture "release server" (the same asset shape the CI workflow publishes: a `<stem>.manifest.json` plus `<stem>.qcow2.zst.NN.part` files): `image list` (human + `--json`, version filter, draft release excluded), `image download` by version/variant and by release tag (verified install, byte-identical payload, manifest provenance, scratch dir removed, re-download reused), a corrupted part refused with the cached image left intact, `--json` progress lines, an unpublished combination reported actionably, and an instance created from the downloaded image |
| `31_oci_export.sh` | `export-oci`: exports an instance disk as an OCI image layout directory (`oci-layout` + `index.json` + `config.json` + a rootfs blob under `blobs/sha256/`) and verifies the `index.json` parses as a manifest list (`manifests[]` with `mediaType`), `--json` output, and a nonexistent-source negative. Skips when `qemu-img` is unavailable.

### Guest package ops: offline (07) + online (23)

The two package-install paths for guests deliberately live in two
different suites because one is the offline rescue path and the other the
default online path. Suite numbers are chronological, not thematic:

- `07_guest.sh` — guest operations, deep path: offline `install`/
  `remove`/`list` via the guestfs appliance (`--offline`), plus Android
  boot-mode / ARM translator via the same appliance.
- `23_online_install.sh` — the default online package path (QGA against
  a booted guest): `install`/`remove`/`exec` without `--offline`.

For the package ops feature, read the two together: `07` covers offline
(rescue, VM won't boot), `23` covers the normal running-VM path.

Every suite is self-contained and order-independent — the orchestrator
starts a fresh daemon + sqlite store per suite, and the harness keeps a
clean `~/.andler/instances` between suites (each suite removes its own
instances). Running suites out of order (e.g. `23` before `07`) is
supported; verified in the containerized run.

### Deep guest tests

The offline guest paths (`guest install/remove/list`, boot-mode switching)
connect the disk image to `/dev/nbd*` and mount guest partitions — this needs
the `nbd` kernel module and `CAP_SYS_ADMIN`. The compose `e2e` service runs
`privileged: true` and mounts `/lib/modules` for `modprobe nbd`, so the suite
has full coverage on a typical Linux host. When the module cannot be loaded
(e.g. restricted kernels, non-Linux Docker hosts), suite 07 prints `SKIP` and
the rest of the suite still runs. To opt out of the privileged mode:

```bash
E2E_PRIVILEGED=false docker compose -f docker/e2e/compose.yaml run --rm e2e
```

Suite 07 builds its guest disk at runtime from a minimal Debian rootfs baked
into the image (via `debootstrap` at image build time) — no pre-made images
are downloaded during the run.

### Why not one target for everything

`andler-core` is a pure domain crate and tests without any environment. Some
`andler-qemu`/`andler-daemon` tests require a real QEMU process and
`/dev/kvm`. Splitting targets means CI on every PR runs the fast unit suite
without dragging in KVM, and the KVM-dependent jobs run on merge.

## Convention for tests requiring external binaries

A test that needs real `/dev/kvm` (andler-qemu) or just the `qemu-img`
binary without KVM (andler-disk) is marked `#[ignore]` with a comment
explaining the reason. The `integration-test` target runs them explicitly via
`cargo test -- --ignored`. The `e2e` target goes further — it starts a real
`andlerd` and drives the CLI end to end.

## Production deployment is NOT Docker

The `daemon` target exists for development experiments, not as a delivery
mechanism for `andlerd` to end users: on a real machine the daemon runs as a
systemd user service with direct access to `/dev/kvm` and the host display
(see `scripts/andlerd.service`).

## Troubleshooting

### KVM: permission denied

```bash
ls -la /dev/kvm          # must exist
sudo usermod -aG kvm $USER && exec $SHELL
```

### Build fails with a protobuf error

`protoc` is installed inside the build image (`protobuf-compiler`) — if the
host build fails instead, install `protobuf-compiler` locally or use Docker.

### Suite 07 always SKIPs

The `nbd` kernel module is not available in the container. Check
`modprobe nbd max_part=8` on the host, or accept reduced coverage —
everything else still runs.

### Clearing Docker cache

```bash
docker builder prune --filter type=exec.cachemount   # cargo caches
docker system prune -f
```
