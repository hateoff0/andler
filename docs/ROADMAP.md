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
| **Guest provisioning** | 🚧 | Zero-root online + offline shipped; appliance-based package install and readiness reporting remain |
| **Resource controls** | ✅ | CPU pinning, mlock, hugepages, overcommit gate, start-time conflict gates |
| **Observability** | ✅ | `/proc` + vendor GPU telemetry, event stream, daemon logs and metrics |
| **Hardware I/O** | 📐 | Isolated network mode, USB device passthrough |

---

## 🚧 Now — active work

- **Readiness ladder, end to end.** The contract is documented in [`docs/ARCHITECTURE.md`](ARCHITECTURE.md): a monotonic ladder `SerialUp → QgaUp → DisplayApplied → GuestOsUp → WaydroidReady`, derived from the effective `(kind, boot_mode)` profile rather than `kind` alone. The event transport (dedicated QMP event monitor + QGA) is **done**; what remains is the guest-side reporting of the display and OS-ready levels so `status` and the guest-access levels consume a real level instead of log text.

- **Config-path convergence.** The wizard, a TOML file, CLI flags, `config set`, and `config edit` must resolve to the *same* config through the *same* validation, so a key added in one place works everywhere. The key schema (`ConfigKey`) now covers the whole `InstanceConfig`; the remaining work is proving every creation path goes through one resolver.

- **Offline package installs through the libguestfs appliance.** Today offline package work runs on the `guestmount` + userns chroot kitchen, while `GuestfsMutator` drives `guestfish` for other mutations. A spike is tracking whether the chroot recipe (guest resolv.conf, `/dev` `/proc` `/sys` binds, tmpfs `/run`, index refresh) transfers into the appliance, so one path serves every offline mutation.

- **Isolated network mode.** `network.mode = "Isolated"` is config-representable and round-trips through proto, but `andler-net::setup_isolated` still returns an explicit *not implemented* error. Needs the netns/veth plumbing that keeps the guest off every host network while still allowing host-side control.

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
<summary><strong>Core engine, graphics, Android, storage, guest ops, observability, isolation, UX, quality gates</strong></summary>

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
- ✅ Zero-root offline path: `guestmount` FUSE + unprivileged user namespaces + chroot; no `/dev/nbd*`, no root, no sudoers.
- ✅ Smart maintenance path (headless auto-start → QGA install → stop), declarative provision manifests, and `guest apply` from the instance's own config.

### Observability
- ✅ `/proc`-based CPU, RSS, disk and network metrics, plus AMD sysfs / NVIDIA NVML / Intel `i915`-`xe` GPU and VRAM fields.
- ✅ QMP event stream (`andler events`), daemon log ring (`andler logs daemon`), daemon metrics (`andler doctor --metrics`), `request_id` correlation, and a documented log-redaction policy.

### Isolation, UX, quality
- ✅ Zero-root guest provisioning — no privileged helper, no sudoers rules anywhere.
- ✅ Start-time gates: host-port conflict, disk already in use, overlapping CPU pins, memory overcommit — each with an actionable message.
- ✅ Unified `create` + interactive wizard, VM templates, `config view|edit|set|status`, `--dry-run`/`--verify`, `--json`, partial instance IDs, shell completions, `doctor`.
- ✅ Unit, integration, gRPC round-trip, and containerized E2E tiers; the four-step pre-merge gate plus per-commit gating on `main`.
- ✅ Release pipeline: a `v*` tag publishes `andler` + `andlerd` archives with `SHA256SUMS` and a build-provenance attestation, so installation is a download rather than a build.
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
