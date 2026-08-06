# docker

Two independent purposes, separate subdirectories — don't confuse them:

| | What | Why |
|---|---|---|
| [`e2e/`](e2e/README.md) | Build and test **ANDLER itself** (daemon/CLI/crates) in containers | Reproducible build + verification: consistent Rust/system lib versions everywhere, unit/integration tests, and the full end-to-end suite over the real daemon and CLI |
| [`images/`](images/README.md) | Build the **guest system base image** (Arch + `linux-cachyos` + Waydroid + weston) for Android/Linux instances | Backing file for qcow2 overlays that actually run under QEMU inside instances |

In short: `e2e/` builds and verifies what ANDLER *is*; `images/` builds what ANDLER *runs inside VMs*. Neither is the deployment channel for `andlerd` to users — production runs as a systemd user service (`scripts/andlerd.service`, data under `~/.andler/`); both subdirectories are build/test infrastructure.