<div align="center">

# Changelog

**ANDLER — Android Linux Emulator & Runtime**

<br/>

[![Keep a Changelog][badge-kac]][kac]
[![Semantic Versioning][badge-semver]][semver]
[![Unreleased][badge-unreleased]](#unreleased)
[![0.1.0][badge-v010]](#010---2026-09-13)
[![GPL-3.0][badge-license]](../LICENSE)

<br/>

**[README](../README.md)** &nbsp;•&nbsp; **[Roadmap](ROADMAP.md)** &nbsp;•&nbsp; **[Architecture](ARCHITECTURE.md)** &nbsp;•&nbsp; **[API reference](API.md)**

</div>

---

> [!NOTE]
> This file follows [Keep a Changelog][kac] and [Semantic Versioning][semver]. Entries describe **user-visible behavior**, not the diff. Full per-commit history lives in `git log`.

| Section | Means |
| :--- | :--- |
| **Added** | New capability, flag, RPC, or config key |
| **Changed** | Existing behavior or default that moved |
| **Fixed** | A bug a user could hit |
| **Removed** | Surface that no longer exists |

---

## [Unreleased]

_Nothing yet._

---

## [0.1.0] - 2026-09-13

**The first release of ANDLER** — a QEMU/KVM control plane that runs Linux and Android virtual machines as managed systems: one daemon, one CLI, one declarative `instance.toml`.

| | |
| :--- | :--- |
| **Guests** | Linux (ISO install) · Android 11 & 13 (Waydroid, VANILLA / GAPPS) |
| **Host** | Linux x86_64 with KVM — everything under `~/.andler/` |
| **Binaries** | `andlerd` (daemon, one supervisor per instance) · `andler` (thin gRPC client) |
| **3D graphics** | Venus (Vulkan) · VirGL (OpenGL) · Virtio-GPU · CPU — no second GPU, no passthrough |
| **Snapshots** | External QCOW2 overlay chains: live create, offline restore, branching |
| **Guest access** | QEMU guest agent online · `guestmount` + unprivileged user namespace offline (zero root) |
| **Telemetry** | CPU/RAM/disk/net from `/proc` · AMD, NVIDIA and Intel GPU metrics, 1 s cadence |
| **License** | GPL-3.0 |

### Highlights

- **🎮 Paravirtualized 3D, not passthrough.** Venus and VirGL share the host GPU over `virtio-gpu` with shared-memory (`blob` + `hostmem`) buffers, so graphics-heavy guests run without dedicating a card. The configured resolution is applied *inside* the guest and can be changed live on a running VM.
- **🤖 Android that actually boots.** Android 11 and 13 base images published as release assets (`andler image list` / `image download`, every part checksum-verified against the release manifest), a Weston/GL compositor that presents on any host GPU, `libndk`/`libhoudini` ARM translation with managed `build.prop` keys, boot-mode switching, and a `base_image_pin` that refuses a swapped backing image.
- **📸 Snapshot trees.** Snapshots are external QCOW2 overlay layers switched over QMP **while the guest runs**; restore is offline and either discards newer layers or archives the whole chain as a branch (`--branch`) you can switch back to; delete commits a layer into its parent. Linked clones protect their chain, and the chain is reconciled on daemon startup so an interrupted operation recovers.
- **🔒 Zero-root guest provisioning.** Online work goes through the guest agent on a private chardev socket; offline work mounts the disk with `guestmount` (libguestfs FUSE) and runs the package manager inside an unprivileged user namespace. There is no privileged helper binary and no sudoers rule anywhere.
- **🛡️ Refused before they hurt.** Starting an instance is gated on host-port conflicts, a disk already in use by another running instance, overlapping CPU pins, and guest RAM that would exceed host physical memory — each with an actionable message instead of an opaque QEMU failure. Long operations are cancellable, and `--idempotency-token` makes a network retry join the operation already running.
- **📊 Observable by default.** CPU, resident RAM, disk and network throughput from `/proc`; VRAM and GPU load from AMD sysfs, NVIDIA NVML or Intel `i915`/`xe`; a QMP event stream (`andler events`), the daemon's own log ring (`andler logs daemon`), and `andler doctor --metrics` for RPC latency and error counts.
- **🖥️ One control plane.** `andler` covers the whole lifecycle — create (wizard, flags or TOML), start/stop/pause/resume, live device hot-plug, clone, export, OCI image export, snapshots, guest packages, diagnostics — with `--json` on every reader for scripting.

### Install

```bash
tar -xzf andler-v0.1.0-linux-x86_64.tar.gz
sudo install -m 0755 andler-v0.1.0-linux-x86_64/andler \
                  andler-v0.1.0-linux-x86_64/andlerd /usr/local/bin/

andler --version          # andler 0.1.0
andlerd --version         # andlerd 0.1.0
andler doctor             # KVM, QEMU, OVMF, zero-root prerequisites, daemon
andlerd                   # or: scripts/install.sh  → systemd user unit
```

The archive ships both binaries, `LICENSE` and `README.md`, with a `.sha256` sidecar and a build-provenance attestation.

### Requirements

Linux with KVM (`/dev/kvm`, user in the `kvm` group), QEMU with OVMF/UEFI support. Offline guest operations additionally want `guestmount`, `/dev/fuse` and unprivileged user namespaces — `andler doctor` names whatever is missing and the command that fixes it. Bridge networking wants `CAP_NET_ADMIN`; `Nat` mode needs nothing extra.

### Known limitations

- **Isolated network mode** is accepted by the config but `andler-net`'s setup is not implemented: it fails with an explicit error rather than pretending to isolate.
- **VFIO passthrough** is reserved in the domain model (`RenderBackend::Passthrough`) but **not implemented** — a start is rejected before QEMU is touched. Paravirtualized Venus/VirGL is the supported 3D path.
- **MicroG** is recorded in the config and nothing installs it yet (no base-image variant ships it).
- **Hot-plugged extra disks** are outside the snapshot chain, which covers the primary disk.
- **Live migration** is not supported; snapshots are per-host.
- **Release artifacts are x86_64 glibc**, built from the tagged commit.

<details>
<summary><strong>Full change list</strong></summary>

### Added

- **One archive per component.** A release publishes `andlerd-<tag>-linux-x86_64.tar.gz` (daemon), `andler-cli-<tag>-linux-x86_64.tar.gz` (client) and `andler-<tag>-linux-x86_64.tar.gz` (both, plus `LICENSE` and `README.md`), each with its own `.sha256`. A host that only serves VMs does not download the CLI, and a client machine can install just the client.
- **`scripts/install.sh` installs from a release.** `--component daemon|cli|both`, `--from-release [TAG]`, `--bin-dir DIR`, `--no-service`: it downloads the archive for the component, verifies the published checksum, installs into `~/.local/bin`, and points the systemd user unit at exactly the binary it installed. `scripts/uninstall.sh` gained `--binaries` for the reverse.
- **Zero-root offline guest operations** — `guestmount` (libguestfs FUSE) + `unshare --user --map-root-user --mount` + chroot replace the qemu-nbd / host-mount / privileged-helper pipeline. The `andler-helper` crate, its `/usr/local/sbin` binary, `doctor --fix`, and every sudoers rule are gone.
- **`ApplyGuestProfile` RPC + `andler guest apply <id>`** — the instance's own config decides the guest-side work (`kind.android_profile.arm_translator` → the translator, `input.clipboard_enabled` → `spice-vdagent`), applied through the state-appropriate path with one classified outcome per selection (`applied` / `already_present` / `skipped` / `failed`). The wizard runs it right after creating a VM.
- **`andler guest provision <manifest> <id>`** — declarative TOML batch (`write`/`upload`/`mkdir`/`cp`/`mv`/`rm-rf`/`chmod`/`symlink`, optional octal modes) through the shared `MutatorOp` batch, online or offline.
- **Base images from GitHub Releases** — `andler image list` / `andler image download` read the release catalog, verify every `.qcow2.zst.NN.part` against the release manifest's sha256, unpack the zstd stream, and install the image plus the published manifest into `~/.andler/cache/base-images/<android>-<variant>/`. Interrupted downloads reuse verified parts; a cached build is reused without a request.
- **`ListRemoteBaseImages` (unary) + `DownloadBaseImage` (server-streaming progress)** — the daemon is the only component that talks HTTP, so the CLI and a future GUI share one route.
- **OCI image export** — `andler export-oci <id> <dest> --disk-format qcow2|raw|vdi` builds a spec-conformant OCI image layout (`oci-layout`, `index.json` manifest list, `config.json`, rootfs blob under `blobs/sha256/`). Source must be qcow2 and in a terminal state.
- **Resource controls** — `memory.mem_lock` (mlock), `memory.hugepages` (hugetlbfs via `memory-backend-file`), and `cpu.affinity` (QEMU launched under `taskset -c`), plus the `MemoryOvercommit` start gate and running-guest-RAM reporting.
- **Autostart** — `autostart = true` starts the instance with the daemon through the same path as `andler start`; a failing autostart never loops.
- **Idempotency tokens** — `--idempotency-token` makes a network retry join the in-flight operation instead of duplicating it; a *different* token while one runs is refused with `OperationAlreadyRunning`; a cancelled operation is never joined.
- **`--json` across the CLI** — `create` (real, `--dry-run`, `--verify`), `clone`, `export`, `disk info`, `guest list`, `config status`, `status`, `doctor`, `exec`, `attach`/`detach`, `op list`, `cache`, `image`; failures emit a single `{"error": "…"}` document on stderr.
- **Observability** — per-request `request_id` correlation (CLI → RPC → operation), `andler logs daemon` (in-memory 4096-line ring; `--follow`, `--json`, `--since`), `GetDaemonMetrics` + `andler doctor --metrics` (RPC latency p50/p99, errors by status code, instance/op counts, running guest RAM), and rate-limited QMP reconnect logging.
- **Smart guest package path** — `guest install`/`remove` on a stopped VM auto-starts it headless, installs through QGA, and stops it again as a cancellable supervisor operation; `--offline` forces the rescue path for a VM that cannot boot.
- **`ConfigKey` schema** — the whole `InstanceConfig` is walked as keys: every key is settable or rejected with the reason it is not (`cpu.affinity` is file-only; `id`/`kind`/`disk.path`/`backend` are immutable). `display.resolution` is the live key.
- **New surfaces** — `andler cache list|clean`, `andler op list|cancel`, `andler events`, `andler connect` (console / ssh / adb / auto), `andler exec`, `andler attach|detach disk|net`, `create --template` (`headless`, `desktop`, plus `~/.andler/templates/`), `network.port_forwards`, boot-mode get/switch, and the `GetVersion` handshake.
- **Log redaction policy** — guest output is never logged at INFO/WARN; documented in `docs/ARCHITECTURE.md`.

### Changed

- Release titles no longer repeat the project name — GitHub titles the release with the tag, which is the version a reader needs — and the release notes open with the same navigation row as the README.
- **Snapshots are external overlay chains** — live create switches the QEMU block graph over QMP to a fresh overlay; restore is offline (discard, or `--branch` archives the chain); delete commits a layer into its parent. Legacy internal snapshots stay listable and deletable.
- **Instance registry is file-based** — `instance.toml` + `events.jsonl` per instance; SQLite keeps snapshot metadata only; legacy databases migrate on first start, and a broken entry is listed (never fatal).
- **Base-image cache layout** — `cache/base-images/<androidN>-<variant>/` subdirectories (the flat root is still read); Android overlay default raised to 128 GiB, disk default 256 GiB.
- **Wizard reworked** — orchestration, questions, request building, presentation, and summary split into modules; content-sized framed panels, a summary grouped by identity/storage/display/CPU/network, and a `--quick` path that deliberately does not trigger translator downloads.
- **Host-image pipeline** — images are published as GitHub **pre-releases**; the downloader reads the API asset URLs (not `browser_download_url`), which is what a private repository needs.

### Fixed

- **`guest install` / `guest apply` inside a running guest timed out after 60 s.** One package-manager step (index refresh, install, remove) now gets a real budget — `ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS`, default 600 s, minimum 30 — and a timeout reports which step it was and how to raise it. A first sync plus a download on a fresh image routinely takes minutes, so the old cap turned a working install into a failure.
- **The online package path never refreshed the package index.** A guest that had never synced (a fresh Android image) failed the install with "target not found", which reads like a missing package. Both paths now run the manager's refresh command from `PackageManager::refresh_args()` — the same single source they already shared for install and remove.
- **`guest list` on an Android VM hid the clipboard agent.** The Android list was the ARM translators only, so `spice-vdagent` — the package `guest apply` installs from `input.clipboard_enabled` — never appeared. The shared packages (clipboard, guest agent, webdav) now apply to both platforms, with the translators layered on top for Android.
- **Image downloads were silent without a terminal.** `andler image download` now prints one progress line per 10 % of the payload when stdout is not a TTY, and the wizard's spinner shows bytes against the total instead of only the asset name.
- Online guest install ran GNU coreutils' `install` instead of the package manager — the guest argv now carries the manager binary.
- A retry during a maintenance auto-start was refused by the lifecycle gate instead of joining the operation already doing the work.
- `create --dry-run --json` printed the preview and then created the instance anyway.
- Base-image catalog parsing: prereleases were filtered out, manifest requests used the wrong `Accept`, and assets were fetched through an unfetchable URL.
- Metrics poller panicked on non-monotonic `/proc/<pid>/io` counters — deltas now clamp at zero.
- Multi-KB guest stderr tripped tonic's h2 client; error messages are sanitized and truncated to 384 chars.
- Offline DNS: the guest `resolv.conf` is written with full `nameserver <ip>` directives (bare IPs are ignored).
- ARM translator staging: the GitHub archive wrapper is flattened so cache hits match, `libhoudini` got a working init script and re-pinned prebuilts, managed `build.prop` keys are cleared on every switch, the ABI list matches the Google/WSA order, and never-booted instances seed `build.prop` from the base image.
- Starting an instance whose disk is already used by a running instance now fails with an actionable message instead of QEMU's opaque write-lock error.
- QMP: stale socket files are removed on spawn, a dropped connection is retried once per operation, and `ProcessNotRunning` distinguishes a dead QEMU from a command error.

### Removed

- Dangling `PLAN.md` references in `scripts/install.sh` and `scripts/andlerd.service`, and the dead `andler-helper` / `/etc/sudoers.d/andler` cleanup in `uninstall.sh --purge` — both belonged to the privileged-helper era that the zero-root migration removed.
- `andler-helper` crate, its binary, `doctor --fix`, and all passwordless-sudo rules.
- Dead stub crates (`frontend/`, `guest-image/`, `packaging/`, `presets/`), the dead `--instance-kind` flag, the `create-android` command (merged into `create`), and the `--libndk` boolean (replaced by `--arm-translator`).

---

### Initial scaffolding

- `andler-core`: domain model — `InstanceConfig`, `HypervisorBackend` trait, FSM, `CloneMode`, `AndroidProfile`, path resolution.
- `andler-qemu`: QEMU backend — process management, QMP client, command-line builder.
- `andler-disk`: qcow2 create/overlay/clone/resize via `qemu-img`.
- `andler-store`: SQLite persistence and legacy migration.
- `andler-rpc`: protobuf definitions and proto ↔ domain conversions.
- `andler-daemon`: instance lifecycle, supervision, log and metrics streaming.
- `andler-cli`: thin gRPC client covering every instance operation, plus the interactive wizard.
- Docker build/test infrastructure and the guest base-image pipelines.

</details>

---

<div align="center">

<br/>

<sub>Format by [Keep a Changelog][kac] · versioned by [SemVer][semver] · maintained with the ANDLER Feature Delivery Loop</sub>

<sub>GPL-3.0 © the ANDLER contributors</sub>

</div>

[kac]: https://keepachangelog.com/en/1.1.0/
[semver]: https://semver.org/spec/v2.0.0.html
[badge-kac]: https://img.shields.io/badge/Keep%20a%20Changelog-1.1.0-6366f1?style=for-the-badge
[badge-semver]: https://img.shields.io/badge/SemVer-2.0.0-0284c7?style=for-the-badge
[badge-unreleased]: https://img.shields.io/badge/status-unreleased-ea580c?style=for-the-badge
[badge-v010]: https://img.shields.io/badge/0.1.0-22c55e?style=for-the-badge
[badge-license]: https://img.shields.io/badge/GPL--3.0-a855f7?style=for-the-badge
