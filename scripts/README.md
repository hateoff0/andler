# scripts

Helper development scripts, not part of the product.

## `start.sh` (removed)

This script was the reference QEMU launch script with 3D acceleration via Venus. It served as the source of truth for `backends/andler-qemu/src/cmdline.rs`. The functionality has been fully migrated to the Rust implementation, and the script has been removed. Any discrepancies between `cmdline.rs` and the original script should be investigated as potential regressions.

## `andlerd.service` / `install.sh`

Systemd **user** service for `andlerd` (see ARCHITECTURE.md, item 9). Deliberately a user unit (`systemctl --user`), not a system unit — `andlerd`'s GPU/audio/display auto-detection and the QEMU windows it spawns all depend on the invoking user's own session (see the comment at the top of `andlerd.service` for the full reasoning). Install with `scripts/install.sh` (run as yourself, not root).
