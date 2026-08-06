# docker

Two independent purposes, separate subdirectories — don't confuse them:

| | What | Why |
|---|---|---|
| [`dev/`](dev/README.md) | Build and test **ANDLER itself** (daemon/CLI/crates) in containers | Reproducible CI environment: consistent Rust/system lib versions, isolated tests requiring `/dev/kvm` from those that don't |
| [`images/`](images/README.md) | Build the **guest system base image** (Arch + `linux-cachyos` + Waydroid + weston) for Android/Linux instances | Backing file for qcow2 overlays that actually run under QEMU inside instances |

In short: `dev/` builds what ANDLER *is*; `images/` builds what ANDLER *runs inside VMs*. Neither is production delivery of `andlerd` to users (that's `.deb`/`.rpm`/AUR/AppImage, see `packaging/`) — both are build infrastructure.
