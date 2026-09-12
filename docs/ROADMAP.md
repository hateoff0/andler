# Roadmap

## Done

- [x] Domain model and backend abstraction (`andler-core`)
- [x] QEMU backend with full lifecycle management (`andler-qemu`)
- [x] QEMU VM snapshots — create/restore/delete/list via job API
- [x] Real-time resource metrics from `/proc` (CPU, RAM, disk, network)
- [x] AMD GPU metrics from sysfs (VRAM, GPU load)
- [x] NVIDIA GPU metrics via NVML + `nvidia-smi` fallback
- [x] Intel GPU metrics via sysfs `i915` (GPU load delta)
- [x] SQLite state persistence with cascade delete (`andler-store`)
- [x] gRPC protocol with 30 RPCs and bidirectional conversions
- [x] Factory reset with file cleanup (`remove --purge`)
- [x] Clone/export for Linux and Android VMs (3 modes)
- [x] Configurable per-instance snapshot timeout
- [x] Snapshot timeout per-operation override (`--timeout` flag)
- [x] Unified `create` command with `--kind linux`/`--kind android` + Android TOML
- [x] Monorepo restructure (core/backends/services/daemon/cli)
- [x] Comprehensive documentation in English
- [x] Disk management CLI (create, info, resize with shrink protection, compact)
- [x] Default disk size changed to 256 GiB
- [x] `--quick` mode — skip wizard, create with all defaults
- [x] `--cdrom-bus` flag (auto/virtio/ide)
- [x] `--compact-on-shutdown` feature (background task)
- [x] `--arm-translator` enum (none/libndk/libhoudini) replacing `--libndk`
- [x] Interactive wizard with smart defaults and hardware auto-detection
- [x] `andler edit` command — edit instance config via gRPC
- [x] Shell completions (bash, zsh, fish)
- [x] Partial instance IDs (Docker-style 8-char hex prefix)
- [x] Unified paths module (`andler-core::paths`)
- [x] `ensure_private_dir()` with 0700 permissions
- [x] `instance.toml` persistence alongside instances
- [x] `purge_instance_files()` with `remove_dir_all`
- [x] `resolve_instance_id()` — Docker-style partial ID resolution
- [x] `create_linux_instance()` — high-level resource creation
- [x] `update_instance_config()` — edit config via gRPC
- [x] `find_live_clones()` — clone source protection
- [x] Signal handling (SIGINT + SIGTERM)
- [x] systemd user unit + installation script
- [x] Colored status output with `IsTerminal` gating
- [x] NVML integration for NVIDIA GPU metrics
- [x] Snapshot limit (20 per instance)
- [x] `MalformedInstanceRef` error for invalid IDs
- [x] `CompactNotApplicable` error for raw format disks
- [x] `ShrinkRequiresConfirmation` error for disk shrink without `--shrink`
- [x] Log history from `qemu.log` before streaming live tail
- [x] Guest package management — `andler guest install/remove/list` with auto-fallback (online via guest agent socket, offline via qemu-nbd)
- [x] Guest image pipelines — automated Android/Linux base image builds with Waydroid (`docker/images/`). Rootfs Dockerfile (Arch + CachyOS kernel + Mesa + waydroid + weston), `build.sh` orchestrator, `build-disk.sh` (rootfs → GPT-partitioned bootable qcow2 with UKI), `fetch-waydroid-images.py` (SourceForge RSS + MD5 verify). Base image auto-discovery via `base_image::resolve()` in `andler-core`. Daemon uses it automatically when client omits `base_image_path`. Android session composites via Weston (GL/virgl) instead of gamescope (Vulkan/venus) so the display path works on any host GPU — see `docker/images/README.md`.
- [x] Base-image downloads — `andler image list` / `andler image download` read the GitHub release catalog the CI workflow (`.github/workflows/build-base-image.yml`) publishes, verify each `*.qcow2.zst.NN.part` against the release manifest's sha256, unpack the zstd stream and install the image (plus the published manifest) into `~/.andler/cache/base-images/<android>-<variant>/`. Exposed as the `ListRemoteBaseImages`/`DownloadBaseImage` gRPC routes so the CLI and a future GUI share one path (the daemon is the only component with network access); `ANDLERD_IMAGE_REPO`/`ANDLERD_IMAGE_API_BASE` point it at a mirror. The wizard offers the download when no local image matches. See `services/andler-disk/src/base_image_download.rs`.
- [x] Guest selections applied automatically — `ApplyGuestProfile` / `andler guest apply` derive the guest-side work from the instance's own config (ARM translator, SPICE clipboard agent) and apply each selection offline (libguestfs appliance) or online (QGA), reporting one classified outcome per selection; the wizard runs it right after creating a VM, so its answers are actually installed instead of printed as a manual next step. See `core/andler-core/src/guest_profile.rs` and `apps/daemon/src/daemon/guest_profile.rs`.
- [x] Network modes in `andler-net`: **bridge** implemented (via iproute2); **isolated** is config-representable but `setup_isolated` returns an explicit "not implemented yet" error — see `services/andler-net/README.md`
- [x] Host-side bridge creation (via iproute2)
- [x] `--dry-run` flag on `andler create` — prints the resolved config and QEMU command line without contacting the daemon at all (client-side resolution mirroring the daemon's own logic: OVMF auto-detect, disk relocation, `andler_qemu::cmdline::build_args`). Covers TOML mode and CLI mode; the interactive wizard already has its own summary screen before creating, so `--dry-run` with a bare `andler create` isn't supported — see `apps/cli/src/preview.rs`.
- [x] `--verify` flag — validates a resolved instance config (paths exist, OVMF found/required-for-Android, disk size sane, GPU memory/CPU/memory in range) and prints a ✓/✗ report, without contacting the daemon. Exits non-zero if any check fails (scriptable). Built on the same client-side resolution as `--dry-run` (`preview::resolve_linux`/`resolve_android`) — see `apps/cli/src/verify.rs`.
- [x] External overlay snapshots — `snapshot create` switches the live disk graph over QMP (`blockdev-add` + `blockdev-snapshot`) to a fresh overlay, turning the previous `disk.qcow2` into a layer under `disk.snapshots/<uuid>.qcow2`; restore is offline and either discards newer layers or (`--branch`) archives the current chain as a branch (`pre-branch-<ts>` head, `branch-<ts>` records, switch-back supported); delete commits the layer into its parent and re-points children; the chain is reconciled against the store on daemon startup (crash-safe across the rename pair). Linked clones protect their source chain (`RestoreWouldBreakClones`/`DeleteWouldBreakClones`). Legacy internal snapshots remain listable/deletable. `ANDLERD_DEV_RESTART=1` keeps VMs alive across daemon restarts.
- [x] QEMU backend: improve QMP error handling and recovery — a dropped/stale QMP connection is cleared on a connection-level error and reconnected once per operation (`pause`/`resume`/`status`). `BackendError::ProcessNotRunning` distinguishes "QEMU process itself is gone" (checked via `is_alive()` before giving up) from a transient QMP hiccup or a genuine command failure (`CommandFailed`/`ParseError`, never retried — QEMU already answered, retrying changes nothing). See `diagnose_and_reset_qmp` in `backends/andler-qemu/src/backend.rs`.
- [x] Core: add disk space pre-check before snapshot operations — checks free space on the disk's filesystem via `statvfs(2)` before calling `backend.snapshot()`, using guest RAM size as a conservative upper bound for vmstate size (exact snapshot size isn't knowable in advance). Fails with `DiskError::InsufficientDiskSpace` (mapped to `Status::resource_exhausted`) instead of letting the operation run out of space partway through. See `services/andler-disk/src/diskspace.rs`.
- [x] Core: add VM health checks — periodic background task (`ANDLERD_HEALTH_CHECK_INTERVAL_SECS`, default 30s, `0` disables) polls every `Running` instance's real backend status; if the process has died outside the normal `stop_instance` path, the FSM record is transitioned to `Error` and persisted, so a crash is visible in `andler status` instead of silently going unnoticed until someone happens to check. See `apps/daemon/src/daemon/health_ops.rs`. **Auto-restart not implemented as an automatic behavior** — manual `andler start` on a stopped/crashed instance works (the FSM accepts `Start` from `Stopped`/`Error`, see the item right below). What's left out is specifically the *automatic, unattended* retry-on-crash policy (attempt limits, backoff) — a product decision to make deliberately, not bundle in silently with a monitoring feature.
- [x] Core: allow restarting a `Stopped`/`Error` instance without recreating it — `andler_core::fsm` accepts `Start` from both (returns to `Starting`, same path as a fresh `Created` instance); `Daemon::start_instance` is generic over the source state. `is_terminal()` means "this run has ended", not "no transitions remain". `backend.rs::spawn` removes a stale QMP socket file before binding a new one (the deterministic per-instance socket path would otherwise collide on restart).
- [x] Offline guest install on fresh images — the mounted guest is prepared for real package-manager runs: guest `/etc/resolv.conf` written with the host's `nameserver` directives (direct write through NOPASSWD `chroot`; a bind-mount fails on the dangling `stub-resolv.conf` symlink), `/dev`/`/proc`/`/sys` bind-mounted, tmpfs on guest `/run` (gpg-agent), and package indexes refreshed (`update`/`makecache`/`-Sy`) before install. `host_nameservers`/`write_guest_resolv`/`bind_host_mounts` in `services/andler-disk/src/nbd.rs`.
- [x] Online guest operations over the QGA chardev socket (`*.qga.sock`, `org.qemu.guest_agent.0`) instead of QMP — QEMU ≥ 9 registers no `guest-*` commands on QMP. Package install/remove, file writes, and live resolution changes (`set_guest_display_resolution`) all talk to `qemu-ga`.
- [x] gRPC error messages sanitized and truncated (384 chars) — long multi-KB package-manager stderr no longer trips the tonic h2 client with "h2 protocol error".
- [x] `config set` whitelist + live display resolution — keys `display.resolution` (any state; applied live to a running guest via QGA and persisted via fw_cfg), `name`, `arm_translator` (stopped).
- [x] QEMU backend: hot-plug disk/network devices — `andler attach disk|net` / `andler detach disk|net` over four new RPCs (`AttachDisk`/`DetachDisk`/`AttachNetwork`/`DetachNetwork`), gated on `Running`/`Paused`. Extra devices persist in `extra_disks`/`extra_networks` in `instance.toml` and are re-created from the command line at boot; QMP `blockdev-add`/`device_add`/`netdev_add`/`device_del` with async-aware detach (retries only on `DeviceInUse`), host tap lifecycle for bridge mode, rollback on failure. Extra disks are not covered by internal snapshots (see Known Limitations in `backends/andler-qemu/README.md`).
- [x] Privilege: single passwordless-sudo entry point — `andler-helper`. New crate `apps/helper` (`andler-helper`, dir named after the existing `apps/cli`/`apps/daemon` pattern), binary `/usr/local/sbin/andler-helper` (root:root 0755, **std-only, zero deps**), one sudoers line `user ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper` replaces the current ten (`modprobe`/`qemu-nbd`/`mount`/`umount`/`chroot`/`mkdir`/`cp`/`mv`/`rm`/`chmod`) that must stay in sync with every privileged call in `offline-disk` — a stale set broke offline installs and `doctor --fix` once overwrote the file instead of merging. Subcommands (each with strict validation, no shell anywhere): `nbd-connect <dev> <img>` (dev regex `/dev/nbd[0-9]+` + free via `/sys/block` size==0/no pid; img realpath + regular file), `nbd-disconnect` (idempotent no-op when already free), `modprobe-nbd` (fixed args, zero input), `mount-partition <dev> <mp>` (`-o rw`; mp last component owned by `SUDO_UID`), `mount-bind <src> <tgt>` (src ∈ `{/dev,/proc,/sys}`), `mount-tmpfs`, `umount <mp>` (lazy; must be a mount we created per `/proc/self/mountinfo`), `chroot-run <mount> <cmd> <args…>` (chroot(2)+execvp, cmd allowlist `apt-get|apt|dnf|pacman|ln`, clean env + `PATH`/`HOME=/root`), `guest-write <mount> <rel-path>` (stdin content, replaces the `sh -c` resolv hack and the `/tmp` temp-file detour in `sudo_write`), `file <mkdir-p|cp-a|mv|rm-rf|chmod> <paths…>` (every path realpath-checked under a managed mount), `sudoers-print` (replaces the `sudo -n chroot / cat` read-back), `--version`/`--help`. Core validation: parse `/proc/self/mountinfo`; a managed mount has `root=="/"` and `source` = nbd device / tmpfs / host bind; everything else must be a realpath prefix of one. Threat model documented honestly in `docs/ARCHITECTURE.md`: the helper narrows and structure-hardens the surface (argument injection, path traversal, symlink confusion, unqualified `chroot`), but a compromised daemon remains root — it is **not** a sandbox; the boundary is against bugs and other users. Install path is `andler doctor --fix` (interactive sudo `install -o root -g root -m 0755`; `install.sh` stays root-less and untouched); `--fix` migrates old 10-rule files in place when every line matches our pattern (foreign lines → refuse, warn), replaces the 10 binary probes with one `--version` check, and `uninstall.sh --purge` additionally removes the helper.

- [x] Long-operation progress and cancellation (`andler op list`/`op cancel`) — the first long op (snapshot restore) runs as a supervisor sub-task that acks immediately and streams weighted phases; one operation per instance (`OperationAlreadyRunning`), cancellation via a watch token checked at per-file phase boundaries, `Operation` events on the bus and in the audit log (`apps/daemon/src/daemon/ops.rs`).
- [x] Guest access levels (`andler connect`) — console attaches to the serial chardev socket in raw mode (works on any running VM); ssh/adb spawn the external client against `network.port_forwards`; `exec` runs commands in the guest through QGA with exit-code relay. `auto` picks by the effective `(kind, boot_mode)` profile.
- [x] `network.port_forwards` — TOML/proto/cmdline chain (slirp `hostfwd=`, passt port list; bridge/isolated rejected), immutable via `config set`.
- [x] boot_mode in the config — `AndroidProfile.boot_mode` with an Android default; switch records it in `instance.toml` and memory; get works in any state (no offline disk mount).
- [x] Version handshake CLI↔daemon — `GetVersion` RPC checked before every command; mismatched daemon builds are rejected with a restart hint instead of an opaque protobuf error.
- [x] `doctor` checks `CAP_NET_ADMIN` — bridge networking runs `ip link` directly in andlerd; the check warns with a fix (`setcap` or Nat mode).
- [x] Package managers in `andler-core` — `PackageManager` enum + command specifications moved out of `andler-disk` (online QGA path and offline path share one spec).
- [x] Core: add VM template system for quick VM creation — `andler create --template <name>` (built-ins `headless`/`desktop`, user templates in `~/.andler/templates/`), merged defaults < template < CLI flags, `--kind linux` only in this phase.

- [x] GuestMutator abstraction — `MutatorOp` batch + `GuestMutator` trait in `andler-core`, `QgaMutator` (online, QGA, zero root) and `GuestfsMutator` (new crate `services/andler-guestfs`, libguestfs appliance, zero root), with a shared conformance suite that runs the same assertions against both real implementations. Package install stays on the chroot kitchen (spike-verified suspended variant).
- [x] QMP event subscription — `QmpClient` background reader task (async events → broadcast, replies → pending oneshot), `QemuBackend` per-instance relay, daemon forwards `DaemonEvent::Qmp`; `StreamEvents` RPC + `andler events [<id>] [--follow] [--json]`.

## In Progress

- [x] Instance registry on files, not SQLite — `instance.toml` per instance under `~/.andler/instances/<id>/` is the single source of truth for config: the daemon re-reads it on every state transition (hand-edited TOML is honored, `config view` shows the file), `Daemon::restore()` scans the instances directory (broken entries are listed and removable, never fatal), and legacy databases are migrated to toml on first start (a conflicting pre-existing toml refuses startup). SQLite keeps only snapshot metadata. Non-purge `remove` deletes the registry entries and keeps the disk; `--purge` deletes everything.
- [ ] Daemon + CLI architecture rework (no more half-finished paths), remaining items:
`config set` whitelist is a partial re-implementation of the TOML schema — every `InstanceConfig` key should be settable or explicitly rejected with a reason; `pointer_mode` is currently not settable at all
every user-visible path (wizard, TOML, CLI flags, config set, config edit) must converge on the same resolved config with the same validation, so a key added in one place works everywhere (the `config edit` dead-end was found exactly this way)
- [x] Offline guest ops zero-root (guestmount FUSE + userns chroot): replace the `qemu-nbd` + host-mount + `chroot` pipeline in `offline-disk` with the `libguestfs` appliance (libguestfs runs its own unprivileged QEMU instance — no `/dev/nbd*`, no host mounts, no kernel module) so offline install/remove, ARM-translator staging, and boot-mode switching need no root at all and the remaining sudoers set (after the `andler-helper` consolidation) can shrink further. Trade-offs to spike before committing: (a) package-manager installs inside the guest — the chroot recipe (`bind_host_mounts`: guest resolv.conf, `/dev`/`/proc`/`/sys` binds, tmpfs on `/run`, `apt-get update` first) does not transfer to the appliance (its own kernel/sys), so the chroot-based apt/pacman/dnf install must be re-verified end-to-end or scoped out in favor of `virt-customize --install` semantics; (b) exclusive-access semantics — today NbdGuard takes a process flock; libguestfs has its own lock/aq abstraction that must not drop that guarantee; (c) the translator staging and boot-mode writes move from overlay paths to the guest layer (E2E expectations in `07_guest.sh` and fixture paths change); (d) dependency weight — static-linked `guestfs-tools` lands as a new runtime dependency. If the chroot-install spike fails, scoped-down version keeps nbd+mount for package installs only and moves translator/boot-mode to libguestfs. **Spike result (phase 0, live host, libguestfs 1.60.1): installroot does not start on this host** — the appliance ships no package manager (`supermin.d/packages`: rpm-tools only; no apt/dnf/pacman), so `virt-customize --install` is dead on Arch and the scoped-down version is confirmed: **libguestfs for file mutations only** (translator staging, boot-mode switch, resolv.conf/build.prop via guestfs write; `system.img` attached as a second drive of the same session — loop-mount not needed, the appliance has no `/bin/mount` anyway), package installs keep the chroot recipe or move online to the QGA mutator (guest runs its own package manager); exclusive access confirmed — a second parallel agent fails to open the image (qemu image lock); one session ≈ 1.7–1.8 s cold, batch sessions per operation package, never per-file. Readiness-threat contract and log policy live in `docs/ARCHITECTURE.md`.
- [x] Core: add VM resource limits (CPU pinning, memory overcommit) — `cpu.pinning`/`cpu.affinity` pins the QEMU process via `taskset -c <set>` with a cross-instance overlap gate (`host CPU N is pinned by running instance …`) and a beyond-host-CPU-count validation; `memory.mem_lock` mlocks guest RAM (`-overcommit mem-lock=on`); `memory.hugepages` backs RAM with hugetlbfs (`memory-backend-file, mem-path=/dev/hugepages, preallocate=true`). All three are config keys (writable only while stopped), validated against host topology, covered by e2e suites `26_affinity`/`27_mem_lock`/`28_hugepages`.
- [ ] QMP event subscription (async events beyond command responses)
- [ ] Dev-mode daemon restart that preserves VMs — `ANDLERD_DEV_PRESERVE_VMS=1` (or similar): SIGTERM/Ctrl+C takes the adopt path instead of gracefully stopping the VMs, so the daily dev loop (Ctrl+C → rebuild → restart) keeps the guest alive. Phase 0 (foundations) made the default explicit — graceful SIGTERM stops VMs, adopt is a crash-only safety net (R9 decision, `docs/ARCHITECTURE.md`) — but the original motivation of the reconnect work was exactly the dev loop; this flips the behavior under an explicit opt-in. Needs a careful release of `kill_on_drop` on the daemon side of a live `QemuProcess` before exit; planned into phase 1b (diskchain), where the supervisor's first long-running command (`RunOp`) lands — not as a bolt-on.
- [ ] CLI: add `--export` flag to export VM as OCI container

## Short-term  

- [ ] USB device pass-through (`usb-host,vendorid=...,productid=...`) — useful
  for both guest kinds (peripherals into a Linux guest, a physical device
  into Android); not yet tracked anywhere
- [ ] `cmdline.rs`: CPU flag accumulator with dedup + conflict detection
  (reject `+flag` after `-flag` already set, instead of silently building a
  broken QEMU arg string) — quickemu's `add_cpu_flag()` is a concrete
  reference; worth doing as part of the `cmdline.rs` split (already flagged
  oversized, see architecture rework)
- [ ] Wizard: verify host RAM/core auto-tiering against quickemu's model
  (RAM: ≥128G host → 32G VM, ≥64→16, ≥16→8, ≥8→4; cores: tiered by
  `nproc`, hard-fail with the exact config key to fix if a guest OS floor
  isn't met) — the wizard already does "hardware auto-detection"; confirm
  whether host-capacity tiering specifically is already covered before
  building it again
- [ ] OVMF firmware-path discovery: walk a prioritized candidate list across
  distro packaging layouts (Debian/Fedora/Nix/Arch put OVMF in different
  places) instead of one fixed path, so `andler-firmware` doesn't silently
  fail on a host it hasn't been tested against — quickemu's
  `get_qemu_share_path()` + OVMF candidate list is a concrete reference

## Medium-term

- [ ] Multi-disk support (snapshot device name parameterization)
- [ ] Live migration between hosts
- [ ] Tauri GUI client

## Long-term

- [ ] GPU passthrough via VFIO (`RenderBackend::Passthrough`)
