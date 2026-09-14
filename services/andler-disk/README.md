# andler-disk

Disk operations for virtual machines: creation, cloning, resizing, compaction — a wrapper around `qemu-img`, plus domain-specific overlay disk logic for Android instances, and zero-root offline guest provisioning through the libguestfs appliance.

## Modules

### `qcow2` — QCOW2 Operations

All functions are `pub async`, wrapping `qemu-img` CLI invocations via `tokio::process::Command`.

| Function | Signature | Description |
|----------|-----------|-------------|
| `create` | `(path: &Path, size_bytes: u64) -> Result<(), DiskError>` | Create a new standalone qcow2 image. Corresponds to `qemu-img create -f qcow2`. |
| `create_with_backing_file` | `(path: &Path, backing_file: &Path, size_bytes: u64) -> Result<(), DiskError>` | Create a qcow2 overlay with backing file. Checks backing file existence before spawning qemu-img. Corresponds to `qemu-img create -f qcow2 -F qcow2 -b`. |
| `create_overlay` | `(path: &Path, backing_file: &Path, size_bytes: u64) -> Result<(), DiskError>` | Same as `create_with_backing_file` but skips the existence check — for restore, where the target layer exists but may be recreated concurrently. |
| `rebase_unchanged` | `(path: &Path, backing_file: &Path) -> Result<(), DiskError>` | `qemu-img rebase -u -F qcow2 -b` — re-points a layer's backing reference *without* reading data (`-u`). Also accepts a not-yet-existing backing path: snapshot staging points an overlay at its future layer file, which only becomes resolvable after the rename pair. |
| `commit_layer` | `(path: &Path) -> Result<(), DiskError>` | `qemu-img commit` — merges the layer's delta into its backing file. |
| `delete_internal_snapshot` | `(path: &Path, tag: &str) -> Result<(), DiskError>` | `qemu-img snapshot -d` for legacy internal snapshots (offline). |
| `chain_from_head` | `(head: &Path) -> Result<Vec<PathBuf>, DiskError>` | Walks qcow2 backing references from the head down to the base (relative paths resolved against the layer's directory). Used for clone-consumer detection and reconciliation. |
| `clone_full` | `(source: &Path, dest: &Path) -> Result<(), DiskError>` | Full independent clone via `qemu-img convert`. Preserves thin-provisioning. |
| `resize` | `(path: &Path, new_size_bytes: u64, allow_shrink: bool) -> Result<(), DiskError>` | Change logical size. Corresponds to `qemu-img resize`. Shrinking (`new_size_bytes` < current) without `allow_shrink = true` returns `DiskError::ShrinkRequiresConfirmation` instead of touching the file — see API.md, "Disk management". |
| `compact` | `(path: &Path) -> Result<(), DiskError>` | Remove free blocks via `qemu-img convert` to temp file + atomic rename. qcow2-only — returns `DiskError::CompactNotApplicable` for other formats (e.g. raw has no reclaimable metadata). |
| `virtual_size_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read logical (virtual) size from `qemu-img info --output=json`. |
| `disk_usage_bytes` | `(path: &Path) -> Result<u64, DiskError>` | Read actual disk usage from `qemu-img info --output=json` (`actual-size` field). |
| `info` | `(path: &Path) -> Result<DiskInfo, DiskError>` | Read full disk metadata (format, virtual size, actual size, backing file) from `qemu-img info --output=json`. |

No dependency on `andler-core` — works with `&Path` directly, knows nothing about `InstanceConfig`.

### `overlay` — Android Overlay Disks

Domain-specific layer over `qcow2::create_with_backing_file` for Android instances.

| Function | Signature | Description |
|----------|-----------|-------------|
| `create_overlay` | `(instance_dir: &Path, base_image_path: &Path, overlay_size_bytes: u64) -> Result<OverlayDisk, DiskError>` | Creates overlay at `<instance_dir>/disk.qcow2`. Returns `BackingFileNotFound` if base image doesn't exist. |
| `factory_reset` | `(instance_dir: &Path, base_image_path: &Path, overlay_size_bytes: u64) -> Result<OverlayDisk, DiskError>` | Deletes existing overlay and recreates from base. Instance must be stopped first. |

**`OverlayDisk`**: `overlay_path` + `base_image_path` (for logging/diagnostics).

`factory_reset` is delete + recreate, not "clear" of the existing file.

### `clone` — Instance Disk Cloning

Three modes for cloning an existing instance's disk into a new disk (not from a shared `base_image` like `overlay.rs` — separate module because of the different backing chain semantics):

| Function | Signature | Description |
|----------|-----------|-------------|
| `linked_clone` | `(source_disk_path: &Path, dest_path: &Path, size_bytes: u64) -> Result<ClonedDisk, DiskError>` | Overlay with `backing_file = source_disk_path`. Cheap/fast (copies no data). Creates dependency: source cannot be purged while linked clones exist. |
| `full_standalone_clone` | `(source_disk_path: &Path, dest_path: &Path) -> Result<ClonedDisk, DiskError>` | Flattens entire backing chain into independent file via `qemu-img convert`. Expensive but fully independent. |
| `shared_base_clone` | `(source_disk_path: &Path, dest_path: &Path, source_base_image: &Path) -> Result<ClonedDisk, DiskError>` | Byte-copy of source disk file (`tokio::fs::copy`). Inherits qcow2 metadata/backing_file, remains thin, physically independent from source. Survives source deletion. |

**`ClonedDisk`**: `disk_path` + `backing_file: Option<PathBuf>` (`None` for full standalone, `Some(source)` for linked, `Some(shared_base)` for shared base).

**Lifecycle:** Live-clone tracking lives at the daemon level, not here — `Daemon::find_live_clones` (in `andler-daemon`) scans registered instances for `config.disk.base_image == source` and blocks purge of a source disk that still has linked clones (`InstanceHasLiveClones`). The disk crate only knows how to *create* the three clone shapes.

### `guest_tools` — Offline Guest Package Management

Checks and manages packages in a stopped instance's guest OS through the
libguestfs appliance: `GuestfsMutator` mounts the disk with `guestfish`, and
`MutatorOp::RunShell` commands run chrooted into the guest as root, so the
guest's own package manager sees its real root filesystem — `dpkg`'s
`access(R_OK|W_OK)` check on `/var/lib/dpkg` included — with no privilege on
the host.

| Function | Signature | Description |
|----------|-----------|-------------|
| `install_agent_offline` | `async (&dyn GuestMutator, &Path, &str) -> Result<(), DiskError>` | Install a package offline: detect, refuse if already installed, then run the install recipe in one appliance batch |
| `remove_agent_offline` | `async (&dyn GuestMutator, &Path, &str) -> Result<(), DiskError>` | Remove a package offline: same gate, then the removal recipe |
| `check_packages_offline` | `async (&dyn GuestMutator, &Path, &InstanceKind) -> Result<Vec<(&GuestPackage, PackageStatus)>, DiskError>` | One probe batch for every known package of the kind |
| `packages_for` | `(kind: &InstanceKind) -> Vec<&'static GuestPackage>` | Shared packages, plus the platform's own |

**`PackageManager`** (from `andler-core`): `Apt` | `Dnf` | `Pacman`, with
`binary_name()`, `install_args(pkg)`, `remove_args(pkg)`,
`refresh_args()`, `check_installed_command(pkg)` — the same command shapes
the online QGA path uses.

**`PackageStatus`**: `Installed` | `NotInstalled`, decided by probing the
package's `binary_checks` paths.

**Session shape.** An install or remove is three appliance sessions (each one
is an appliance boot, ~2 s warm):

1. **Detect** — one `probe_paths` batch for `/usr/bin/apt-get`,
   `/usr/bin/dnf`, `/usr/bin/yum`, `/usr/bin/pacman`; the first hit picks the
   manager, and none of them is `DiskError::PackageManagerNotFound`.
2. **Gate** — the manager's own query (`dpkg -l` / `rpm -q` / `pacman -Qi`)
   as a shell command in the appliance: exit 0 means installed. An install of
   an installed package ends as `AgentAlreadyInstalled`, a removal of a missing
   one as `AgentNotInstalled`, both without touching the disk further.
3. **Recipe** — a single `apply` batch, so nothing is spread over sessions.
   For an install, in order:
   - the appliance's QEMU user networking, brought up on `eth0` itself
     (`169.254.2.15/16`, gateway `169.254.2.2` — the link-local subnet
     libguestfs hands its appliance). The appliance's boot-time DHCP needs a
     host `dhcpcd` that is not always installed, and a package session with no
     address fails every mirror download;
   - a resolver: a tmpfs on `/run`, `nameserver 169.254.2.3` (slirp's resolver
     above the gateway) written there and bind-mounted over the guest's
     `/etc/resolv.conf`. The appliance bind-mounts its own read-only
     `resolv.conf` there — a file with no nameservers when its DHCP never ran —
     so the session replaces it for its duration. The tmpfs also gives
     `gpg-agent` (pacman signatures) a writable `/run`, and nothing of this is
     written into the guest image;
   - the index refresh (`apt-get update` / `dnf makecache` / `pacman -Sy`), so
     a guest that never synced still installs;
   - the install itself (`apt-get install -y` / `dnf install -y` /
     `pacman -S --noconfirm`). For apt, `-o Acquire::ForceIPv4=true`: the
     appliance's network is IPv4-only;
   - for a package whose `GuestPackage::systemd_unit` is set, the
     `multi-user.target.wants` symlink (`deb-systemd-helper` cannot enable a
     unit without a running systemd, which is always the case here).

   A removal runs the same network and resolver steps and then
   `remove_args` — no index refresh.

**Why the appliance and not a FUSE chroot.** `dpkg` verifies its database
directory with `access(R_OK|W_OK)`, and a `guestmount` FUSE mount denies that
check to a non-root process on some kernels (observed on cachyos 7.6 with
libfuse2: `dpkg` aborts with "required read/write access to the dpkg database
directory"). Inside the appliance the same command runs as real root with the
guest's own root as `/`, so the check passes and the host stays unprivileged.
The phase-0 spike that concluded "installroot does not work in the appliance"
tested `virt-customize --install`, which needs a package manager *inside the
appliance* (an Arch supermin appliance has none); running the *guest's* manager
chrooted into the guest is a different thing and is what this module does —
verified live against the Android 13 base image (`guest install
spice-vdagent`, 10 s end to end).

**Input safety.** The recipe is shell text the guest runs, so a package name
is accepted only if every character is alphanumeric or one of `._+-,:=@`
(`DiskError::InvalidPackageName` otherwise) — the online path passes argv to
`guest-exec` and never had this constraint.

**Known Packages** (`KNOWN_PACKAGES`): `spice-vdagent` (`/usr/bin/spice-vdagentd`), `qemu-guest-agent` (`/usr/bin/qemu-ga`), `spice-webdavd` (`/usr/bin/spice-webdavd`).

**Android Packages** (`ANDROID_PACKAGES`): `libndk` (`var/lib/waydroid/overlay/system/lib/libndk_translation.so`), `libhoudini` (`var/lib/waydroid/overlay/system/lib/libhoudini.so`).

### `boot_mode` — Android Boot Mode Switching

Repoints the guest's `etc/systemd/system/default.target` symlink between the Android and Linux target units on a mounted disk.

| Function | Signature | Description |
|----------|-----------|-------------|
| `switch_boot_mode` | `(overlay_path: &Path, mode: AndroidBootMode) -> Result<(), DiskError>` | Verify the target unit exists, then `ln -sfn` the `default.target` link through the mutator. Fails with `BackingFileNotFound` if the disk doesn't exist. |
| `current_boot_mode` | `(overlay_path: &Path) -> Result<AndroidBootMode, DiskError>` | Read the current target from the mounted disk (`default.target` → Android target ⇒ Android, anything else ⇒ Linux; error if the link is missing). |

The guest's `/etc/systemd/system` is root-owned 755, so the link is replaced through the mutator (offline: the appliance, as root; online: QGA), not by a host-side `symlink` call on a mount; `ln -sfn` both removes the old symlink and creates the new one in one step.

### `diskspace` — Free Disk Space Pre-check

Checks free space on a path's filesystem via `statvfs(2)` before an operation that could otherwise fail partway through with a raw ENOSPC — see ROADMAP.md, "Core: add disk space pre-check before snapshot operations".

| Function | Signature | Description |
|----------|-----------|--------------|
| `check_available_space` | `(path: &Path, required_bytes: u64) -> Result<(), DiskError>` | Fails with `InsufficientDiskSpace` if fewer than `required_bytes` are free on `path`'s filesystem (resolved to its nearest existing ancestor if `path` doesn't exist yet) |

Uses `f_bavail` (blocks available to an unprivileged user), not `f_bfree` (which includes root-reserved blocks the daemon may not actually be able to use, e.g. ext4's default 5% reservation).

**Rationale:** QCOW2 snapshots grow the same disk file — a nearly-full filesystem can fail mid-write. `required_bytes` is a conservative estimate that accounts for worst-case qcow2 internal fragmentation during snapshot creation.

### `nbd` — removed

NBD-based mounting is gone from the crate: `nbd.rs` (qemu-nbd + host mount
+ `sudo andler-helper chroot-run`) was first replaced by `guestmount`
(libguestfs FUSE) with an unprivileged-user-namespace chroot, and that
kitchen was in turn retired when offline guest work moved onto the
libguestfs appliance (`GuestfsMutator`, `MutatorOp::RunShell`). No module in
the crate mounts anything on the host, and `guest_offline.rs` no longer
exists.

### `arm_translator` — ARM Translation Layer Management

Switches ARM translation libraries (libndk / libhoudini) in a mounted Android disk image.

| Function | Signature | Description |
|----------|-----------|-------------|
| `switch_translator_with` | `(mutator: &dyn GuestMutator, translator: ArmTranslator, translator_dir: Option<PathBuf>, android_version: &str) -> Result<TranslatorSwitch, DiskError>` | Replace the current ARM translator in a mounted disk. No-op if the requested translator is already active. **Atomic**: new files are staged into `system/.andler-translator-staging` on the guest fs first (copy + rename), the old translator (`libndk_translation.so` / `libhoudini.so` and friends) is removed only after staging succeeds, then staged files are renamed into place and the staging dir removed. File lists support `lib/libndk*`-style wildcards, expanded against the actual archive/cache contents. Props: every key in `MANAGED_PROP_KEYS` is first removed from `build.prop`, then the translator's props are written (sorted); a missing `build.prop` (never-booted instance) is fine and starts from an empty set. `translator_dir` points at a local extracted cache (skips the download); otherwise the translator is obtained from the version-keyed cache or downloaded + MD5-verifi… |

### `translator` — Translator Metadata

Static metadata about each supported ARM translator (download links, files to install, properties to set).

| Function | Signature | Description |
|----------|-----------|-------------|
| `resolve` | `(translator: ArmTranslator) -> TranslatorInfo` | Look up download links (version-keyed: per Android version a `(version, url, md5)` triple), file lists, property overrides, and init.rc content for a translator variant. |
| `dir_name` | `(translator: ArmTranslator) -> &'static str` | Return the cache directory name for a translator (`"ndk"`, `"houdini"`, or `"none"`). |

**`TranslatorInfo`**: `dl_links`, `files`, `props`, `init_rc`, `detect_file`.

**`MANAGED_PROP_KEYS`**: every `build.prop` key the translators own — used by `switch_translator` to strip stale keys (including `ro.vendor.enable.native.bridge.exec[64]` and `ro.ndk_translation.version`) before writing the new set, so switching `libndk` → `libhoudini` → `None` never leaks props.

**`houdini::INIT_RC`**: a functional replacement for the archive's init script (byte-identical to the waydroid-helper reference): mounts `binfmt_misc` on `early-init` and registers the `arm_{exe,dyn}` / `arm64_{exe,dyn}` handlers via `echo … > /proc/sys/fs/binfmt_misc/register` once `ro.enable.native.bridge.exec=1` is set.

### `translator_download` — Translator Download & Cache

Downloads and caches ARM translator archives (ZIP files) under `~/.andler/cache/arm-translators/` (see `andler_core::paths::arm_translators_dir`).

| Function | Signature | Description |
|----------|-----------|-------------|
| `ensure_translator` | `(translator: ArmTranslator, android_version: &str) -> Result<PathBuf, DiskError>` | Ensure the translator is downloaded and extracted. Returns the cache path (`<arm-translators>/<dir_name>`). Skips the download if the detect file already exists in cache; otherwise picks the URL/MD5 for the requested `android_version` from `TranslatorInfo::dl_links` (error if no link matches), downloads, verifies the MD5 (`BadChecksum` path), and unzips. Extraction flattens the GitHub `<repo>-<commit>/prebuilts/` archive wrapper, so `bin/`/`etc/`/`lib/`/`lib64/` land directly under the cache root (this is also what makes the detect-file cache hit work). |

### `base_image_download` — Published Base Images

Reads the release catalog published by the project's CI (`.github/workflows/build-base-image.yml`) and installs a build into the local base-image cache.

| Function | Signature | Description |
|----------|-----------|-------------|
| `ImageSource::from_env` | `() -> ImageSource` | `ANDLERD_IMAGE_REPO` (default `hateoff0/andler`), `ANDLERD_IMAGE_API_BASE` (default `https://api.github.com`) and `ANDLERD_IMAGE_TOKEN` (falling back to `GH_TOKEN`/`GITHUB_TOKEN`); `with_token`/`has_token` for callers that resolve the credential themselves |
| `invalidate_catalog_cache` | `()` | Drops the in-process catalog cache (index + manifests, 5-minute TTL) that keeps one `image list` + `image download` pair from spending a request per scanned release twice |
| `list_remote` | `async (&ImageSource, &ImageFilter) -> Result<Vec<RemoteImage>, DiskError>` | Read the releases JSON, keep the `base-image-android*` tags (drafts skipped, prereleases accepted — the pipeline publishes every build as one), fetch each release's `<stem>.manifest.json` and build the installable descriptor. Newest build per (Android version, package set) first. A release with an unreadable manifest or a missing part asset is skipped with a warning when other releases are readable, and reported as an error naming it when it is the only one (an empty catalog would otherwise read as "nothing has been published"). |
| `fetch_base_image` | `async (&ImageSource, &RemoteImage, bool, &ProgressSink) -> Result<FetchOutcome, DiskError>` | Download every part (sha256-verified while streaming, restart-on-failure, verified leftovers of an interrupted run reused), concatenate them into one zstd stream, unpack into `<stem>.qcow2.partial` while hashing, compare against the manifest's `sha256`, then rename the image and write the manifest bytes exactly as published into `~/.andler/cache/base-images/<android>-<variant>/`. `force` re-downloads an already-cached build; without it, a cached build is returned as `reused` without any request. |
| `installed_image` | `(manifest_id: &str) -> Option<BaseImageInfo>` | The cached build with that manifest id, if any |

Split seam for the next pass: this module is past the 600–800-line mark that
the repo treats as a design signal. It has two jobs that do not share state —
reading the catalog (`ImageSource`, the release-JSON types, `list_remote`, the
repository/single-asset payload shaping) and fetching one build
(`fetch_base_image`, per-part download, zstd unpack, verification, atomic
install) — and the test module follows the same split (fixture server and
catalog parsing vs install/reuse/corruption), so `base_image_download/{mod,listing,fetch}.rs`
is the split to make, not a merge candidate.

Design notes: the parts are byte ranges of a *single* compressed stream (GitHub caps a release asset at 2 GiB), so they are concatenated — `ChainedReader` — before decompression rather than decompressed individually; a release that publishes one uncompressed `<stem>.qcow2` is supported too, and refused when its manifest carries no sha256 (an unverifiable image is never installed). Progress goes through a clonable `ProgressSink` because unpacking runs on the blocking pool while the download loop runs on the async runtime. Network I/O is bounded: 15 s connect, 60 s per-read stall guard, 3 attempts per asset.

### ARM Translation — Design Notes, Fix History & Reference Comparison

How `guest install libndk`/`libhoudini` works, what was broken before, and
how the reference implementations do it. Last cross-verified against
waydroid-helper and waydroid_script on 2026-08-07.

#### Mechanism

1. Resolve the translator's `FILES` / `PROPS` / `INIT_RC` / `DETECT_FILE`
   (`translator/{ndk,houdini}.rs`).
2. Without `--translator-dir`, `ensure_translator()` downloads the
   version-keyed archive (`11`/`13`), verifies its MD5, and extracts it
   into `~/.andler/cache/arm-translators/<dir_name>/`; extraction flattens
   the `<repo>-<commit>/prebuilts/` wrapper so the payload sits directly
   under the cache root.
3. The instance disk is mounted through the libguestfs appliance and the
   target becomes `/var/lib/waydroid/overlay/system/`
   (created if missing — waydroid merges this overlay over `/system` at
   container start, so a never-booted instance is supported).
4. If the target's `DETECT_FILE` already exists, the install is a no-op
   ("already installed, skipping").
5. **Stage** — every `FILES` entry (wildcards like `lib/libndk*` are
   expanded against the source dir) is copied into
   `system/.andler-translator-staging/`; `bin`-component paths get 0755.
   Missing concrete entries log a warning and are skipped (the built-in
   `houdini.rc` covers houdini's missing archive rc).
6. **Remove** the previous translator's files (same expansion, against the
   system dir) and its `<dir_name>.rc`.
7. **Move** staged files into place, drop the staging dir.
8. **Props** — the merge always starts from `base_build_prop()`: the guest's
   pristine build.prop *below* the overlay — a plain `system/build.prop`, or
   (waydroid mainline layout) `/system/build.prop` extracted with `debugfs`
   from `etc/waydroid-extra/images/system.img` (e2fsprogs, essential; no root
   to read the image). The existing upper file is **never** a source — it is
   derived data and reinstalling regenerates it, so a partial upper written
   by an older buggy build is repaired instead of perpetuated. Without any
   base source the install fails loudly (an upper with only translator props
   would shadow the base's `ro.*` wholesale and break Android boot). Every
   key in `MANAGED_PROP_KEYS` is removed, then the translator's `PROPS` are
   written (sorted).
9. **Init script** — if `INIT_RC` is `Some`, it overwrites
   `etc/init/<dir_name>.rc` (parent dir created).

#### Properties

`MANAGED_PROP_KEYS` is the union of every key any translator owns (incl.
`None`), so switching libndk → libhoudini → None can never leak stale
keys (`ro.ndk_translation.version`, `ro.vendor.enable.native.bridge.exec[64]`,
`ro.dalvik.vm.native.bridge`, the abilist trio, `ro.dalvik.vm.isa.*`,
`ro.enable.native.bridge.exec`).

`ro.product.cpu.abilist` uses the order `x86_64,arm64-v8a,x86,armeabi-v7a,armeabi`
— the same order the official Google images ship (ChromeOS zork
ndk_translation and WSA houdini `prop.json`). waydroid_script hardcodes the
legacy `x86_64,x86,arm64-v8a,…` order; the difference only affects apps that
ship both x86 and arm64 variants (which ABI wins the ranking).

#### Init scripts

- **houdini** — `INIT_RC` is a functional rc, byte-identical to the
  waydroid-helper reference `houdini.rc` (pinned by the test
  `houdini_init_rc_matches_reference_binfmt_registration` against
  `tests/fixtures/houdini.rc`): it mounts `binfmt_misc` on `early-init`
  and, once `ro.enable.native.bridge.exec=1` is set, registers the four
  handlers (`arm_{exe,dyn}`, `arm64_{exe,dyn}`) by `exec`ing
  `/system/bin/sh -c "echo ':arm_exe:M::\\\\x7f…' >> register"`. The
  `\\x7f` ELF-magic escapes are written with **four** backslashes because
  they are unescaped twice before the kernel sees them: AOSP init's
  tokenizer halves them (`\\`→`\`, verified against android11/13
  `init/tokenizer.cpp`), the single-quoted shell keeps them verbatim, and
  mksh's `echo` (escape processing on by default) halves them again, so
  the register write carries canonical `\x7f`, which binfmt decodes to
  the ELF magic bytes.
- **ndk** — the archive's own `etc/init/ndk_translation.rc` is kept; it
  `copy`s its rules into `binfmt_misc` itself.

#### Pins (all MD5-verified against the downloaded archives)

| Translator | Android | Commit | MD5 |
|---|---|---|---|
| NDK (`libndk`) | 11 | `9324a8914b649b885dad6f2bfd14a67e5d1520bf` | `c9572672d1045594448068079b34c350` |
| NDK (`libndk`) | 13 | `68734c52556d3d7a6db34c603dd9276915c29f2f` | `0b2207c490fcb400aa5c87fcf0d52d38` |
| Houdini (`libhoudini`) | 11 | `cf7f970f6004f0c329b0464e3d65f9b0e2baea91` | `5554b11cba905058c3d9bb5e45535d83` |
| Houdini (`libhoudini`) | 13 | `debc3dc91cf12b5c5b8a1c546a5b0b7bf7f838a8` | `cb7ffac26d47ec7c89df43818e126b47` |

NDK pins are identical to waydroid-helper's `ndk_translation-chromeos_zork-R136`
package. The houdini pins are the waydroid-helper wsa11 (11) and hpe-14 (13)
commits; their payloads are **byte-identical** (per-file MD5 diff) to the
archives waydroid_script pins (`81f2a51e…` for 11, `9e778963…` for 13) — the
only difference is that those ship a `etc/init/houdini.rc` stub, which ANDLER
overwrites with its own functional rc anyway.

#### Fixed defects (audit 2026-08-07, all fixed in the same pass)

The install flow was live-audited against real archives and a sandbox
instance before the fixes; the defects found and how they were fixed:

| # | Defect | Fix |
|---|---|---|
| 1 | NDK payload incomplete: `etc/binfmt_misc/*`, `etc/cpuinfo.arm{.64}.txt`, `etc/ld.config.arm{.64}.txt`, `bin/ndk_translation_program_runner_binfmt_misc_arm64` were missing from `FILES` — the archive rc `copy`s its rules from `etc/binfmt_misc`, so registration failed | entries added to `ndk.rs::FILES` |
| 2 | `lib/libndk*` / `lib64/libndk*` wildcards never matched (literal `exists()` check) — the native-bridge library, all proxy shims, the detect file were never installed; NDK installs were never idempotent | wildcard expansion in `resolve_entry_paths()` for stage/move/removal |
| 3 | Online path broken: the zip's `<repo>-<commit>/prebuilts/` prefix broke both the cache-hit check and the install joins — default `guest install` silently copied nothing and re-downloaded every run | `flatten_prebuilts()` in `extract_zip` |
| 4 | Houdini rc was an empty stub (trigger lines, no bodies) that overwrote the archive's working rc — binfmt rules never registered | functional rc (waydroid-helper reference, byte-identical) |
| 5 | Houdini pins matched neither reference ecosystem | rebased on waydroid-helper wsa11/hpe-14 commits (payload-identical to waydroid_script's pins) |
| 6 | Old payloads survived a switch (archive `ndk_translation.rc`, `bin/arm`, `bin/arm64` leftovers) | removal now uses the same expansion; stale rc (`<dir_name>.rc`) is always removed |
| 7 | Props leaked between translators (`ro.ndk_translation.version`, `ro.vendor.*` stayed after switching to houdini) | `MANAGED_PROP_KEYS` remove-then-set on every switch, incl. `None` |
| 8 | Fresh install hard-errored when `build.prop` was absent (never-booted instance) | the upper `build.prop` is always regenerated from the base below the overlay: plain `system/build.prop`, or `/system/build.prop` extracted with `debugfs` from `etc/waydroid-extra/images/system.img`; no base source is a hard error, never a silent partial upper |
| 9 | Privilege surface: the install needed NOPASSWD `cp/mkdir/mv/rm/chmod` beyond the documented NBD set | all guest-fs mutations go through the `file`/`guest-write` subcommands of a single privileged **`andler-helper`** binary (sole NOPASSWD rule, `apps/helper`); `andler doctor --fix` installs the binary and migrates the old per-binary rules away. **Superseded twice:** the helper, `doctor --fix` and every sudoers rule were removed when offline mutations became zero-root (`guestmount` + unprivileged user namespaces), and that FUSE kitchen was itself retired when offline package work moved into the libguestfs appliance |
| 10 | **Live boot failure: Android stuck at boot after a translator install.** The upper overlay `build.prop` contained only the 10 translator props and shadowed the base's full build.prop wholesale — waydroid could not parse the Android version from the merged rootfs (`Failed to parse android version from system.img: invalid literal for int() with base 10: ''`) and ART's `derive_classpath` aborted, so the container never finished booting | `base_build_prop()` extracts the base's `/system/build.prop` from `system.img` via `debugfs` and merges translator props into the full file; the upper is regenerated on every install (never read back), so already-broken installs are repaired by re-running `guest install` (or `config set arm_translator none`); E2E 07 crafts a `system.img` fixture and asserts the merged upper; regression tests `base_build_prop_reads_plain_layout_and_errors_without_source`, `base_build_prop_prefers_plain_layout_over_system_image`, `parse_build_prop_skips_blank_and_comment_lines` |

#### Reference comparison (waydroid-helper / waydroid_script)

- **waydroid-helper** installs by copying the entire `prebuilts/*` tree into
  `$pkgdir/system` (an Arch `package()`), applies props through waydroid's
  property layer, and overwrites `houdini.rc` with the functional rc. ANDLER
  does the same with an explicit file list (equivalent — the list mirrors the
  archive payload; on the Android 11 pin it also keeps `bin/arm/linker` and
  `bin/arm64/linker64`, while the Android 13 pin lacks `bin/arm`/`bin/arm64`
  entirely and the expander skips the missing entries with a warning),
  writes props into the overlay `build.prop` directly
  (no guest-side tooling needed), and ships the same rc.
- **waydroid_script** (casualsnek) copies the whole tree, writes props into
  `waydroid.cfg` (applied by a `resetprop` helper at boot), and embeds the
  same rc (modulo a cosmetic leading newline). Its prop key sets are exactly
  ANDLER's; its uninstall does **not** clean props (ANDLER's managed-key
  removal is stricter). Its abilist order is the legacy one (see Properties).
- Both references' pins and ANDLER's are mutually consistent, as verified by
  MD5 and per-file diffs of the actual archives (2026-08-07).

#### Reproducing the live verification

Sandbox recipe used for the audit (all temp, removed afterwards): build a
2 GiB qcow2 with an ext4 root partition, run `andlerd` on a scratch port with
`ANDLER_HOME` pointed at a temp dir (offline guest work needs only
the libguestfs appliance — no root, no helper),
create an Android 13 instance with
that disk as base, then `andler guest install libndk`/`libhoudini` and mount
the overlay to inspect `bin/`, `etc/`, `lib/`, `lib64/`, `build.prop` and
`etc/init/houdini.rc` (md5-compare the rc against `tests/fixtures/houdini.rc`).

## Error Types

**`DiskError`**:

| Variant | Fields | Description |
|---------|--------|-------------|
| `SpawnFailed` | `io::Error` | `qemu-img` binary not found or no exec permissions |
| `CommandFailed` | `status: i32`, `stderr: String` | `qemu-img` exited non-zero |
| `BackingFileNotFound` | `PathBuf` | Backing file doesn't exist (checked before spawning) |
| `ParseError` | `String` | Failed to parse `qemu-img info --output=json` |
| `Io` | `path`, `source` | Filesystem error at path |
| `FileSystem` | `String` | Generic filesystem operation failure |
| `OfflineGuestFailed` | `String` | A step of the offline appliance path failed (batch recipe, appliance, download) |
| `ShrinkRequiresConfirmation` | `path`, `current_size_bytes`, `requested_size_bytes` | Refusing to shrink without `--shrink` flag |
| `CompactNotApplicable` | `path`, `format` | Compact only works on qcow2 disks |
| `PackageManagerNotFound` | `disk: PathBuf` | No known package manager binary in the guest filesystem |
| `AgentAlreadyInstalled` | `package: String` | Package already installed in guest |
| `AgentNotInstalled` | `package: String` | Package not found in guest for removal |
| `InvalidPackageName` | `package: String`, `reason: String` | The package name cannot be handed to the guest's shell (see `guest_tools`) |
| `GuestAgentUnavailable` | `instance_id: String` | Guest agent (qemu-ga) not available for online operations |
| `InsufficientDiskSpace` | `path`, `required_bytes`, `available_bytes` | Not enough free space on `path`'s filesystem for the operation (pre-checked, not a failure mid-operation) |
| `NoGuestOs` | `path` | The disk holds no filesystem libguestfs can inspect (an ISO-install VM before the OS is installed); the appliance's "no operating system was found" is turned into this type so callers classify it instead of pattern-matching text |
| `ImageIndex` | `url`, `message` | The base-image release catalog could not be read (unreachable API, HTTP error, unparseable release index) |
| `ImageDownload` | `asset`, `message` | One published asset could not be transferred (request failure, HTTP error, truncated body, retries exhausted) |
| `ImageVerify` | `message` | A downloaded part, or the unpacked image, does not match the sha256 its manifest declares |

## Tests

### Without `qemu-img` / `/dev/kvm`

- **`qcow2`**: JSON field parsing (`parse_json_u64_field` — compact/spaced/missing/zero/nested-field-ignored; `parse_json_string_field` — compact/spaced/missing/empty; `parse_json_optional_string_field` — value/missing/empty).
- **`clone`**: `shared_base_clone_reports_missing_source_as_io_error`.
- **`boot_mode`**: `read_boot_mode_recognizes_android_target`, `read_boot_mode_treats_anything_else_as_linux`, `read_boot_mode_errors_when_no_default_target_link`.
- **`diskspace`**: `available_bytes_on_temp_dir_is_nonzero`, `available_bytes_resolves_to_nearest_existing_ancestor`, `check_available_space_passes_for_a_tiny_requirement`, `check_available_space_fails_for_an_absurd_requirement`.
- **`guest_tools`**: `install_recipe_runs_every_step_in_order`, `install_recipe_enables_only_the_units_the_package_ships`, `install_recipe_for_apt_keeps_the_ipv4_override_and_drops_the_userns_sandbox`, `remove_recipe_removes_without_refreshing_the_index`, `package_names_the_guest_shell_would_interpret_are_refused`, `install_runs_the_whole_recipe_in_one_appliance_batch`, `install_of_a_package_the_query_reports_leaves_the_disk_alone`, `remove_of_a_package_the_query_reports_missing_leaves_the_disk_alone`, `a_guest_without_a_package_manager_is_named_not_guessed`, `a_disk_without_a_guest_os_keeps_its_own_error`, `a_failed_install_reports_the_appliance_and_the_disk`, `offline_listing_answers_every_package_from_one_probe_batch`, `android_packages_include_the_shared_clipboard_agent`, `linux_packages_are_the_shared_set_without_translators`, `known_packages_has_entries` (all against a fake `GuestMutator` that records the recipe).
- **`nbd`**: `lock_path_for_is_colocated_and_hidden`, `acquire_disk_lock_succeeds_on_a_fresh_path`, `acquire_disk_lock_is_reentrant_within_the_same_process`, `find_free_nbd_device_returns_existing_device_or_explains_absence` (environment-tolerant), `unique_mount_name_is_unique`, `find_root_partition_picks_the_last_partition_not_the_esp`, `find_root_partition_errors_on_empty_list`.
- **`arm_translator`**: `detect_current_translator_returns_none_on_empty_dir`, `build_prop_content_sorts_keys_and_appends_newline`, `resolve_entry_paths_expands_wildcards_in_parent_dir`, `resolve_entry_paths_returns_empty_for_unmatched_wildcard`.
- **`translator_download`**: `extract_zip_flattens_repo_prebuilts_prefix`, `flatten_prebuilts_does_not_touch_already_flat_payload_dirs`, `flatten_prebuilts_errors_on_collision_instead_of_silently_skipping` (plus offline download/checksum error paths).
- **`arm_translator`**: `detect_current_translator_returns_none_on_empty_dir`, `build_prop_content_sorts_keys_and_appends_newline`, `resolve_entry_paths_expands_wildcards_in_parent_dir`, `resolve_entry_paths_slash_star_matches_whole_directory`, `resolve_entry_paths_returns_empty_for_unmatched_wildcard`, `base_build_prop_reads_plain_layout_and_errors_without_source`, `base_build_prop_prefers_plain_layout_over_system_image`, `parse_build_prop_skips_blank_and_comment_lines`.
- **`base_image_download`**: real HTTP against a local fixture server (a hand-rolled listener serving the release JSON, the manifest and the `.part` assets): `list_remote_reads_the_published_catalog`, `list_remote_filters_by_version_and_variant`, `list_remote_skips_a_release_with_a_missing_part_asset`, `fetch_installs_a_verified_image_into_the_cache` (byte-identical payload, manifest provenance, scratch dir removed, progress phases and byte counters), `fetch_reuses_an_already_installed_build_without_touching_the_network`, `fetch_reuses_verified_parts_after_an_interrupted_download`, `fetch_replaces_a_cached_build_when_forced`, `fetch_refuses_a_part_that_does_not_match_its_manifest`, `fetch_refuses_a_payload_that_does_not_match_the_manifest_sha256`, `fetch_reports_an_unreachable_host_and_installs_nothing`, plus `chained_reader_concatenates_parts_in_order` and the env/filter parsing tests.
- **`translator`**: `resolve_ndk_has_links`, `resolve_houdini_has_links`, `resolve_none_has_no_links`, `dir_names` (asserts `"ndk"`/`"houdini"`/`"none"`), `managed_prop_keys_cover_every_translator_key`, `houdini_init_rc_matches_reference_binfmt_registration` (byte-identical to `tests/fixtures/houdini.rc`, the waydroid-helper reference).

### With `qemu-img` (integration tests, `#[ignore]`)

- **`qcow2`**: `create_then_virtual_size_round_trips`, `create_with_backing_file_fails_fast_on_missing_backing`, `resize_grow_succeeds_without_confirmation`, `resize_shrink_without_confirmation_is_rejected`, `resize_shrink_with_confirmation_succeeds`, `compact_qcow2_succeeds`, `compact_raw_is_rejected_as_not_applicable`.
- **`clone`**: `linked_clone_points_at_source_instance_disk`, `full_standalone_clone_has_no_backing_file`, `shared_base_clone_does_not_depend_on_source_after_copy` (explicitly deletes source after copy, verifies clone survives).
- **`overlay`**: `create_overlay_points_at_given_base_image`, `create_overlay_fails_when_base_image_missing`, `factory_reset_recreates_overlay`.

All marked `#[ignore]` — run in `integration-test` Docker target.

## What Is NOT Here

- Network configuration — that's `andler-net`.
- Instance lifecycle management — that's `andler-daemon`.
