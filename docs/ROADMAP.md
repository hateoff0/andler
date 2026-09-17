<div align="center">

# Roadmap

**Where ANDLER is, and where it is going**

<br/>

[![Now][badge-now]](#-now--active-work)
[![Next][badge-next]](#-next--short-term)
[![Later][badge-later]](#-later--medium-and-long-term)
[![Shipped][badge-shipped]](#-shipped)

<br/>

**[README](../README.md)** &nbsp;•&nbsp; **[Changelog](CHANGELOG.md)** &nbsp;•&nbsp; **[Architecture](ARCHITECTURE.md)** &nbsp;•&nbsp; **[API reference](API.md)**

</div>

---

> [!NOTE]
> Statuses are honest, not aspirational: **Shipped** means it is on `main`, covered by the four-step gate and the E2E suites. Tracks are not dated — ANDLER ships when a change passes the gate, not on a calendar.

| Marker | Status | Meaning |
| :---: | :--- | :--- |
| ✅ | **Shipped** | On `main`, gated, E2E-covered where user-visible |
| 🚧 | **In progress** | Active track with concrete remaining work |
| 📐 | **Planned** | Scoped, not started |
| 💡 | **Exploring** | Trade-offs under evaluation, no commitment |

---

## Milestone tracks

| Track | Status | Scope |
| :--- | :---: | :--- |
| **Control plane** | ✅ | `andlerd` + one supervisor per instance, thin CLI, gRPC + version handshake |
| **Graphics & display** | ✅ | Venus · VirGL · Virtio-GPU · CPU, five display engines, live guest resolution |
| **Android** | ✅ | Android 11 & 13, VANILLA/GAPPS, Weston compositor, ARM translators, boot-mode switch |
| **Storage & snapshots** | ✅ | External overlay chains, branching, three clone modes, OCI export |
| **Guest provisioning** | ✅ | Zero-root online (guest agent) + offline (libguestfs appliance), readiness ladder, declarative provision manifests |
| **Networking** | ✅ | NAT (slirp/passt), bridge, and isolated mode in its own user + network namespace |
| **Resource controls** | ✅ | CPU pinning, mlock, hugepages, overcommit gate, start-time conflict gates |
| **Observability** | ✅ | `/proc` + vendor GPU telemetry, event stream, daemon logs and metrics |
| **Hardware I/O** | 📐 | USB device passthrough |

---

## 🚧 Now — active work

Nothing is open here. The four tracks that were in progress all shipped, each with its own tests and a live check:

- the **readiness ladder** (`SerialUp → QgaUp → DisplayApplied → GuestOsUp → WaydroidReady`) is produced by real probes and published on the event bus — `status` prints `readiness: SerialUp of GuestOsUp` on a booting Linux guest, and `connect` waits for the profile's terminal level instead of guessing from log text;
- **offline package installs** run through the libguestfs appliance only: one zero-root path serves every offline mutation, `guest install --offline spice-vdagent` installs into a stopped instance in ~10s with the VM never started, and `guest list` reports it;
- **isolated network mode** builds a user + network namespace whose tap is the guest's only interface — the guest's QEMU has no route to any host network, and `stop` leaves nothing behind;
- **config-path convergence** resolves flags, a TOML file and the wizard's answers through one resolver, so the same input yields the same config (and the same validation outcome) whichever front-end produced it.

What is next is the short-term list below.

---

## 📐 Next — short term

- **USB device passthrough** — `usb-host,vendorid=…,productid=…` for both guest kinds: a peripheral into a Linux guest, a physical device into Android. Needs static declaration plus hot-plug/detach parity with the existing disk and network hot-plug.
- **CPU-flag accumulator in `cmdline.rs`** — dedup and conflict detection (reject `+flag` after `-flag` instead of emitting a broken QEMU argument list), landed together with the module split by device category that its current size calls for.
- **Wizard host-capacity tiering** — scale default RAM and cores to the host (and hard-fail with the exact config key when a guest OS floor is not met), instead of a single fixed default.
- **MicroG as a real path** — today `--microg` is recorded in the config and nothing installs it. Either a `MICROG` base-image variant in `docker/images/` or a first-boot guest step that installs and registers the MicroG APKs; the wizard stays quiet about it until one half exists.
- **Multi-disk snapshot parameterization** — external snapshots currently cover the primary disk only; extra disks attached at runtime are outside the chain.

---

## 💡 Later — medium and long term

- **Live migration** between ANDLER daemons over secure gRPC streams — the barrier today is the non-migratable device set (virtio-sound, virgl/venus display), documented as a Known Limitation in `backends/andler-qemu/README.md`.
- **Tauri desktop client** — a lightweight cross-desktop GUI on the same gRPC surface the CLI uses (the daemon is designed as the single HTTP-speaking component precisely so a GUI can reuse it).
- **VFIO GPU passthrough** — `RenderBackend::Passthrough { gpu_pci_id }` is reserved in the domain model and proto but is **not implemented**: `is_implemented()` returns false and a start is rejected with `InvalidConfig` before QEMU is touched. Paravirtualized Venus/VirGL is the supported 3D path.

---

## ✅ Shipped

<details>
<summary><strong>Everything <a href="CHANGELOG.md">0.1.0</a> ships — core engine, graphics, Android, storage, guest ops, observability, isolation, UX, quality gates</strong></summary>

### Core engine
- ✅ `HypervisorBackend` trait — hypervisor-agnostic contract; QEMU is the implementation.
- ✅ Per-instance supervisor task: the single writer of FSM state, backend handle, and config; readers use `watch` snapshots, mutations go through an acknowledged command channel.
- ✅ File-based registry — `instance.toml` is the source of truth, re-read on every transition; `restore()` tolerates broken entries and migrates legacy SQLite configs.
- ✅ Crash adoption — a surviving QEMU is reconnected via its QMP socket (`query-processes`, `/proc` cmdline marker, identity check) and re-wrapped around its pidfd; only then does an instance fall back to `Stopped`.
- ✅ pidfd death notification, periodic health checks (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`), autostart, event bus, `GetVersion` handshake, single-daemon flock.

### Graphics & display
- ✅ Venus (Vulkan), VirGL (OpenGL), Virtio-GPU, and CPU render backends over `virtio-gpu` with `blob` + `hostmem`.
- ✅ Display engines: SDL, GTK, SPICE, D-Bus, headless.
- ✅ Resolution applied inside the guest (QEMU `fw_cfg` on boot) and switched live on a running VM through the guest agent — Linux session apply, Android compositor restart, VM keeps running.

### Android
- ✅ Android 11 & 13 bootable base images, VANILLA and GAPPS variants, from a release catalog with sha256-verified part downloads and install provenance.
- ✅ Weston DRM/GL compositor (presents on any host GPU, unlike a Vulkan-only compositor on NVIDIA), with Venus still enabled for guest Vulkan apps.
- ✅ ARM → x86 translation: `libndk` and `libhoudini` with managed `build.prop` keys, correct ABI ordering, and re-pinned prebuilts.
- ✅ Boot-mode get/switch on the unified base image; `base_image_pin` (id + sha256) recorded at creation and enforced.

### Storage & snapshots
- ✅ External QCOW2 overlay snapshots: live create over QMP, offline restore, `--branch` archiving with switch-back, delete-commits-into-parent.
- ✅ Chain reconciliation on daemon startup (crash between the rename pair, staging overlays, orphaned layers) and linked-clone protection on restore/delete.
- ✅ `statvfs` free-space pre-check; linked / full-standalone / shared-base clone modes; standalone export; OCI image export.

### Guest operations
- ✅ Online path via the QEMU guest agent on a private `*.qga.sock` chardev (install/remove, exec, file writes, resolution).
- ✅ Zero-root offline path: one libguestfs appliance session per batch (`guestfish`), the guest chrooted inside it, no FUSE mount, no root, no sudoers.
- ✅ Readiness ladder (`SerialUp → QgaUp → DisplayApplied → GuestOsUp → WaydroidReady`) derived from the effective `(kind, boot_mode)` profile, produced by probes rather than parsed from log text, published on the event bus and shown by `status`; `connect` waits for the profile's terminal level.
- ✅ Offline package install/remove/list through the same appliance, so the online and offline paths agree: `guest install --offline <pkg>` works on a stopped instance with the VM never started, and `guest list` reports what landed.
- ✅ One appliance boot per operation instead of one per question: the appliance boots once per mutator and stays listening, so `guest install libndk` is 7.7s (was 20.8s, and 4m10s before the per-file work went away) with two boots instead of nine.
- ✅ Smart maintenance path (headless auto-start → QGA install → stop), declarative provision manifests, and `guest apply` from the instance's own config.

### Networking
- ✅ NAT (slirp and passt, with port forwards), bridge mode, and isolated mode.
- ✅ Isolated mode gives the guest its own user + network namespace: the tap inside it is the guest's only interface, there is no route to any host network, and QMP/QGA stay reachable because they are UNIX sockets. Teardown leaves no namespace or tap behind, and `doctor` reports what the host is missing when it cannot provide one.

### Observability
- ✅ `/proc`-based CPU, RSS, disk and network metrics, plus AMD sysfs / NVIDIA NVML / Intel `i915`-`xe` GPU and VRAM fields.
- ✅ QMP event stream (`andler events`), daemon log ring (`andler logs daemon`), daemon metrics (`andler doctor --metrics`), `request_id` correlation, and a documented log-redaction policy.

### Isolation, UX, quality
- ✅ Zero-root guest provisioning — no privileged helper, no sudoers rules anywhere.
- ✅ Start-time gates: host-port conflict, disk already in use, overlapping CPU pins, memory overcommit — each with an actionable message.
- ✅ Unified `create` + interactive wizard, VM templates, `config view|edit|set|status`, `--dry-run`/`--verify`, `--json`, partial instance IDs, shell completions, `doctor`.
- ✅ One resolver for every creation front-end: flags, a TOML file and the wizard's answers produce the same config and the same validation outcome (proved by a test that runs all three through the same draft).
- ✅ Unit, integration, gRPC round-trip, and containerized E2E tiers; the four-step pre-merge gate plus per-commit gating on `main`.
- ✅ Release pipeline: a `v*` tag publishes one archive per target triple (`andler-<tag>-x86_64-unknown-linux-gnu.tar.gz`, both binaries), the raw `andler`/`andlerd` binaries for hosts that want one of them, and `SHA256SUMS` over the set, with a build-provenance attestation — so installation is a download rather than a build.
- ✅ OVMF discovery across distro packaging layouts via a prioritized candidate list (Arch, Debian/Ubuntu, Fedora, openSUSE, `qemu` layouts).
- ✅ `ANDLERD_DEV_RESTART=1` — dev restart leaves VMs running and the next daemon start adopts them.

</details>

---

<div align="center">

<br/>

<sub>Roadmap owned by the ANDLER maintainers · statuses verified against `main` at each release</sub>

<sub>GPL-3.0 © the ANDLER contributors</sub>

</div>

[badge-now]: https://img.shields.io/badge/now-active%20work-6366f1?style=for-the-badge
[badge-next]: https://img.shields.io/badge/next-short%20term-0284c7?style=for-the-badge
[badge-later]: https://img.shields.io/badge/later-medium%20%26%20long-6366f1?style=for-the-badge
[badge-shipped]: https://img.shields.io/badge/shipped-22c55e?style=for-the-badge
