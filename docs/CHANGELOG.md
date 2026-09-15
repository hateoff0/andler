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

### Changed

- **The wizard's prompts are typed, and the dialog reads as one session.** Every question is now built from the answer's own value instead of a label string that was parsed back — GPU backend, audio backend, ARM translator, CD-ROM bus, network mode, VM type, configuration mode, Android version and the summary action all carry their value through, so the wizard cannot disagree with itself about what an option means. The label tables and the `parse_*_choice` helpers that re-derived the value from the text are gone, as is the option reordering that existed only to place the default first.
- **Every option explains itself where it is highlighted.** An entry is a short label plus a hint (`Venus` — `Vulkan 3D — fastest`), and the recommended answer is the one the cursor opens on rather than a `(recommended)` suffix in the label; the multi-select for advanced groups lists what each group owns instead of a label with the detail bolted on in parentheses.
- **The wizard is framed and speaks one language.** The session opens with `andler · create a virtual machine` and closes with `The instance is ready.`; a cancelled prompt is followed by `Nothing was created.` (previously a bare `Cancelled.` on its own line, which said nothing about the outcome); steps, warnings and progress all use the same symbol set as the prompts.
- **Wizard progress is a bar, and the wizard no longer owns a progress renderer.** The base-image download shows transferred bytes against the total with an ETA, creation and the guest-selection apply are spinners, and the ~60-line hand-rolled ANSI writer that drew `\r` progress lines is removed. The download bar uses a template sized for an 80-column terminal instead of the stock one, which spends 98 columns on the same line and made the live frame wrap. Nothing is drawn when stderr is not a terminal, so `--quick` under a pipe stays silent where it used to print progress lines. The questions need a terminal on stderr now: `andler create 2>log` refuses with the non-TTY usage error instead of drawing prompts into the log file.
- **Every line the wizard draws fits the terminal.** A prompt's hint sits on the same line as its label and is not wrapped by the prompt library, so the hints, the question lines and the progress messages were sized for 80 columns rather than left to the terminal's mid-word wrap — which, for a redrawn frame, left the wrapped remainder on screen. The daemon's messages (a catalog error, a failed download) keep their full text by moving to a wrapped log line instead of the one-line progress frame.
- **`andler image download` reports through the same bar.** The command drew its own `indicatif` bar with the full asset name on every frame; it now feeds the same bar the wizard uses — phase line, byte counters and ETA — so `image download` and the wizard's guest-image question render the identical thing for the identical stream. The non-interactive half is unchanged: without a terminal on stderr it prints the phase transitions and one line per 10 % of the payload (and `--json` still emits one document per progress line).
- **The wizard's report names the instance the way the rest of it does.** The daemon's per-selection messages carry the full 64-character id, which the panel had to break mid-token; they are shown with the short id, like the `start`/`connect` lines right below.
- **The wizard's dialog has a hierarchy now.** The question being asked is bold, the entry under the cursor is bold, and the answers, hints and placeholders are dim — cliclack's own theme colours only the symbols, which left a screen of equally-weighted grey text. The wizard's own questions are lowercase (`which VM do you want to create?`, `disk size (GiB)`), matching the panel labels it already used. `NO_COLOR` still strips all of it.
- **Downloads show their speed.** The base-image progress bar reports the transfer rate and an ETA next to the byte counters, on both `andler image download` and the wizard's guest-image question, and sizes its bar to the terminal it is drawn on.
- **One prompt library for the whole CLI.** The wizard, `remove --purge` and `snapshot delete` confirmations are built on the same prompt engine, whose spinner and progress bars are the `indicatif` engine the rest of the CLI already renders with; `inquire` is no longer a dependency.
- **A VM name is trimmed at the prompt.** Leading and trailing spaces used to be accepted by the validator and then fail the daemon's own config validation; the name the operator typed is what is stored.

### Fixed

- **`Change some settings` could not change the CD-ROM bus.** The modify pass handed the group's previous answer straight back instead of offering it as the default, so picking "Boot & disks" re-asked the compact-on-shutdown question but silently kept the old bus. Every question now treats a previous answer as its default, exactly like the rest of the advanced groups.
- **Creation from a multi-GB base image took a minute.** The base-image pin derives the image's sha256 on every create, and the hashing ran at 48 MB/s in the profile a developer builds; the digest is now memoized per `(path, size, mtime)` — replacing the image still changes one of those and re-hashes — and the `sha2` backend is built with its SHA-NI path, which took the single hash from 51 s to 1.2 s in release. Repeated creates from the same image: 54 s → 2 s (debug), and the cached-image case is now dominated by the appliance boot.

---

## [0.1.0] - 2026-09-13

_Published as [`v0.1.0`](https://github.com/hateoff0/andler/releases/tag/v0.1.0): archives with checksums and a build-provenance attestation, installed by [`scripts/install.sh`](https://github.com/hateoff0/andler/blob/main/scripts/install.sh)._

**The first release of ANDLER** — a QEMU/KVM control plane that runs Linux and Android virtual machines as managed systems: one daemon, one CLI, one declarative `instance.toml`.

| | |
| :--- | :--- |
| **Guests** | Linux (ISO install) · Android 11 & 13 (Waydroid, VANILLA / GAPPS) |
| **Host** | Linux x86_64 with KVM — everything under `~/.andler/` |
| **Binaries** | `andlerd` (daemon, one supervisor per instance) · `andler` (thin gRPC client) |
| **3D graphics** | Venus (Vulkan) · VirGL (OpenGL) · Virtio-GPU · CPU — no second GPU, no passthrough |
| **Snapshots** | External QCOW2 overlay chains: live create, offline restore, branching |
| **Guest access** | QEMU guest agent online · libguestfs appliance offline (zero root) |
| **Telemetry** | CPU/RAM/disk/net from `/proc` · AMD, NVIDIA and Intel GPU metrics, 1 s cadence |
| **License** | GPL-3.0 |

### Highlights

- **🎮 Paravirtualized 3D, not passthrough.** Venus and VirGL share the host GPU over `virtio-gpu` with shared-memory (`blob` + `hostmem`) buffers, so graphics-heavy guests run without dedicating a card. The configured resolution is applied *inside* the guest and can be changed live on a running VM.
- **🤖 Android that actually boots.** Android 11 and 13 base images published as release assets (`andler image list` / `image download`, every part checksum-verified against the release manifest), a Weston/GL compositor that presents on any host GPU, `libndk`/`libhoudini` ARM translation with managed `build.prop` keys, boot-mode switching, and a `base_image_pin` that refuses a swapped backing image.
- **📸 Snapshot trees.** Snapshots are external QCOW2 overlay layers switched over QMP **while the guest runs**; restore is offline and either discards newer layers or archives the whole chain as a branch (`--branch`) you can switch back to; delete commits a layer into its parent. Linked clones protect their chain, and the chain is reconciled on daemon startup so an interrupted operation recovers.
- **🔒 Zero-root guest provisioning.** Online work goes through the guest agent on a private chardev socket; offline work chroots the guest inside a libguestfs appliance session (`guestfish`), one session per batch, with the guest's resolver and the package manager running in the same command because a mount does not survive from one line of a session into the next. There is no privileged helper binary, no sudoers rule and no FUSE mount anywhere.
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

Linux with KVM (`/dev/kvm`, user in the `kvm` group), QEMU with OVMF/UEFI support. Offline guest operations additionally want `guestfish` (`libguestfs-tools` / `guestfs-tools`) — `andler doctor` names whatever is missing and the command that fixes it. Bridge networking wants `CAP_NET_ADMIN`; `Nat` and `Isolated` need nothing extra (`Isolated` needs unprivileged user namespaces and `/dev/net/tun`, both checked).

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

- **The guest readiness ladder is real, end to end.** `SerialUp → QgaUp → DisplayApplied → GuestOsUp → WaydroidReady` is produced by probes — the process/serial state, the guest agent, the resolution the guest reports having applied, the guest's own init/OS-ready report and, on Android-in-waydroid-mode, the container's state — and published on the event bus as `EventKind::Readiness`. The levels a run can reach come from the effective `(kind, boot_mode)` profile, so an Android guest switched to Linux boot-mode ends at `GuestOsUp` instead of waiting for a Waydroid that will never start; `status` prints the level it reached and the level the profile ends at (`readiness: SerialUp of GuestOsUp`), and a level the profile cannot observe is never counted. Nothing decides readiness from log text any more.
- **Isolated network mode.** `network.mode = "isolated"` starts the guest's QEMU inside a fresh unprivileged user + network namespace whose tap is the guest's only interface: no route to any host network, no host interface created, and QMP/QGA stay reachable because they are UNIX sockets. The namespace is validated before the guest starts (exactly `lo` plus the tap, no IPv4 route) and a host that cannot provide one is refused with an actionable message rather than degraded; `stop` leaves no namespace or tap behind, and `doctor` names what the host is missing.
- **One resolver for every creation front-end.** CLI flags, a TOML file and the wizard's answers now build the same draft and go through the same resolution and validation, so a key added in one place works in all of them; the test that proves it runs all three and compares the resolved config and the validation outcome.

- **One archive per component.** A release publishes `andlerd-<tag>-linux-x86_64.tar.gz` (daemon), `andler-cli-<tag>-linux-x86_64.tar.gz` (client) and `andler-<tag>-linux-x86_64.tar.gz` (both, plus `LICENSE` and `README.md`), each with its own `.sha256`. A host that only serves VMs does not download the CLI, and a client machine can install just the client.
- **`scripts/install.sh` installs from a release.** `--component daemon|cli|both`, `--from-release [TAG]`, `--bin-dir DIR`, `--no-service`: it downloads the archive for the component, verifies the published checksum, installs into `~/.local/bin`, and points the systemd user unit at exactly the binary it installed. `scripts/uninstall.sh` gained `--binaries` for the reverse.
- **Zero-root offline guest operations** — a libguestfs appliance session (`guestfish`) replaces the qemu-nbd / host-mount / privileged-helper pipeline, and later the `guestmount` FUSE mount with it. The `andler-helper` crate, its `/usr/local/sbin` binary, `doctor --fix`, and every sudoers rule are gone.
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

- **Every guest operation now shows what it is doing.** `guest install`/`remove`, `guest apply`, `guest provision`, `guest list`, `guest boot-mode` and the translator switch print the daemon's operation phase, its percentage and the elapsed seconds while they run, instead of a bare "this can take a while" and then silence for minutes. The line is refreshed on every change *and* every two seconds, so a slow step ticks visibly instead of looking frozen; the translator download reports real bytes against the total, and the daemon logs each stage, so `andler logs daemon` shows the same progression. The translator switch (a libguestfs session, an ~18 MiB download and three appliance batches) runs as a tracked operation with named stages — downloading, opening the guest disk, staging, removing the old translator, updating the properties — so it is visible on `andler events` and cancellable. Steps that are not operations (an offline `guest list` mounting the disk inline, for example) report their own label and elapsed time, because silence was what made them look hung.
- **External guest steps are bounded.** The libguestfs appliance (`ANDLERD_GUESTFS_TIMEOUT_SECS`, default 300 s, raised to a package budget for package sessions), one guest command (`ANDLERD_GUEST_COMMAND_TIMEOUT_SECS`, default 900 s) and one package-manager run (`ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS`, default 600 s) have wall-clock limits: a wedged appliance or mount used to hang the operation forever with nothing in the log, and now fails with the step, the elapsed bound and the variable that raises it.
- Release titles no longer repeat the project name — GitHub titles the release with the tag, which is the version a reader needs — and the release notes open with the same navigation row as the README.
- **Snapshots are external overlay chains** — live create switches the QEMU block graph over QMP to a fresh overlay; restore is offline (discard, or `--branch` archives the chain); delete commits a layer into its parent. Legacy internal snapshots stay listable and deletable.
- **Instance registry is file-based** — `instance.toml` + `events.jsonl` per instance; SQLite keeps snapshot metadata only; legacy databases migrate on first start, and a broken entry is listed (never fatal).
- **Base-image cache layout** — `cache/base-images/<androidN>-<variant>/` subdirectories (the flat root is still read); Android overlay default raised to 128 GiB, disk default 256 GiB.
- **Wizard reworked** — orchestration, questions, request building, presentation, and summary split into modules; content-sized framed panels, a summary grouped by identity/storage/display/CPU/network, and a `--quick` path that deliberately does not trigger translator downloads.
- **Host-image pipeline** — images are published as GitHub **pre-releases**; the downloader reads the API asset URLs (not `browser_download_url`), which is what a private repository needs.

### Fixed

- **An offline guest operation paid for the appliance boot once per question.** A libguestfs session costs about two seconds before it does anything, and a translator install asked nine of them — 22s of appliance time for a 21s operation, with the actual work taking a fraction of a second. `GuestfsMutator` now boots the appliance once and keeps it listening: every later call is a cheap client of the same booted session. `guest install libndk` went from 20.8s to **7.7s**, with two boots instead of nine and a per-call cost of ~10ms instead of ~2s; the remaining seconds are the boot and the in-guest work (unpacking and moving the 43 MB payload). The session is stopped with the mutator, a listener left behind by a killed daemon is reaped on the next boot, and a session that ends with a command it rejected is rebooted — with the old one stopped first, so it cannot keep the image lock the new one needs.

- **Offline package installs failed on the guest's mirrors.** Three faults in one path: the appliance's own resolver is the host's copied with loopback entries dropped, so a host that resolves through systemd-resolved's stub left the appliance with no resolver (the host's real upstream servers are now discovered and passed through, IPv4 first); a resolver mounted by its own session line is gone by the time the next line runs, because every line of a libguestfs session has its own mount namespace (the setup now travels in the same command as the package manager); and the translator probe paths in `guest list` were relative, which the appliance refuses with `path must start with a / character`. Verified end to end on a real Android image: `guest install --offline spice-vdagent` on a stopped instance in ~10s with the VM never started, `guest list` then reporting it, and `guest remove --offline` taking it back out.
- **A refused ARM-translator change rewrote the disk anyway.** `config set <id> kind.android_profile.arm_translator <value>` changes the translator on the disk image — which is why the key is immutable — but the refusal arrived *after* the switch, so the command failed with the translator already removed while `instance.toml` still named one. The key is refused before anything touches the disk now, so `config set` on it leaves the disk and the config exactly as they were. In the same area, `guest install none` tried to install a Debian package called `none` (it booted a maintenance VM and failed with "target not found: none"): a translator name now means the same thing to `install` and `remove` — `none` disables ARM translation through the removal path, with no download.
- **A running guest operation froze on its first stage — or named the wrong one.** Two faults stacked. The CLI worked the stage name back out of the progress percentage, and the earliest stage sitting on that number won (a cached translator payload skips the download, so staging — not downloading — is the stage that runs). Underneath that, the registry `ListOperations` answers from — what `andler op list`, `guest install`, `guest apply` and every progress line poll — held a *copy* of the operation taken when it was accepted, while the runner advanced a private one, so no phase ever arrived and the percentage never moved: `guest install none` printed `downloading the translator (0%)` every two seconds for a whole run whose daemon log showed `opening the guest disk` → `staging the translator files` → `removing the old translator` → `updating the translator properties`. The phase the daemon entered now travels in `current_phase` on `OperationInfo` and the CLI prints it (the derived name is kept only for a daemon built before the field), and the supervisor and the runner share **one** operation through a `tokio::sync::watch` channel, so the listed entry advances with the work instead of staying at the values it was accepted with.
- **`guest install libndk` failed outright on a real machine** with `guestfish: command arguments not separated by whitespace`. The staging command was built with single quotes, and guestfish's script parser delimits an argument with single quotes and rejects a quote that follows a closing one — a quoted command cannot be expressed at all. The command is escaped instead of quoted (spaces and shell metacharacters get a backslash, which guestfish passes through to the guest's shell), and a path that would need a quote is refused with an explicit error rather than mangled.
- **A translator switch spent minutes in staging.** It ran three guestfish operations per payload file, and a host-side move loop asked the appliance `exists()` twice per file — each `exists()` is its own guestfish run, i.e. its own appliance session, so libndk's ~180 files cost ~530 operations plus ~354 sessions, and a session costs about a second whatever it carries. The payload now travels as one gzipped tar that the guest unpacks with a single command, the move is one in-guest `cp -a`, and the old translator is removed by the guest's shell (the previous cleanup resolved the outgoing translator's guest paths against the *host* filesystem, so libndk's `lib/libndk*` and `lib64/libndk*` were skipped and its libraries stayed in the guest). Measured on a real Android instance: staging went from 3m47s to **5s** and `guest install libndk` from 4m10s to **26s**, in 12 appliance sessions instead of ~360. Each appliance session logs its duration at `debug` — that is how the cost was found, and it is what to read next time a guest operation is slow. `MutatorOp::RunShell` is internal: the proto carries it so the mapping stays total, but the daemon's request path refuses it, because a manifest must not be able to run arbitrary shell in the guest.
- **A guest install opened the appliance more often than the work needed.** An appliance session costs roughly two seconds before it does anything, and the install path asked one question per session wherever it could have asked several: finding where Waydroid keeps its overlay took a session per candidate, detecting the installed translator one per translator, and the move into place was a session of its own. Probing is now a batch (`GuestMutator::probe_paths`, one script for every path, with the answers matched by position), and the move rides in the staging batch. Measured on a real Android instance: `guest install libndk` went from 12 appliance sessions and 26s to 9 sessions and 19s, with the staging phase down from 5s to 2s (it was 3m47s before the per-file operations went away).
- **The wizard called the translator `Libndk`.** Its "installed in the guest right after creation" list printed the Rust variant name next to the lowercase `spice-vdagent`. The list is lowercase and aligned now — a label column and a detail column — and names the translator the same way the CLI flag, the config and the logs do.
- **`guest install` / `guest apply` reported success for work that had not happened.** A retry — or the wizard's apply step — whose idempotency key matched an operation already in flight returned the moment the *join* succeeded, so `Translator `libndk` installed` printed while the staging batch was still running, and `guest list` kept reporting "not installed". A joined caller now waits for the operation to finish and reports its real outcome (a joined operation that fails surfaces as `OperationFailed`); the translator switch, the package install and the package remove all follow that rule. Verified against a real Android guest: the switch ran for 3m59s, claimed success only after the properties were written, and `guest list` then reported `libndk installed`.
- **One spelling for the ARM translator.** `Libndk` / `Libhoudini` (the variant names) appeared in the wizard summary, `status`/`config view` and the daemon logs next to the lowercase names used everywhere else; every user-visible name is lowercase now, including the log fields (`translator=libndk`).
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
