# scripts

Helper development scripts, not part of the product.

## `start.sh`

Reference QEMU launch script with 3D acceleration via Venus. This is the source of truth for `backends/andler-qemu/src/cmdline.rs` — any discrepancy between what `cmdline.rs` generates and this script should be investigated as a potential regression.

## `andlerd.service` / `install.sh`

Systemd **user** service for `andlerd` (see PLAN.md, item 9). Deliberately a user unit (`systemctl --user`), not a system unit — `andlerd`'s GPU/audio/display auto-detection and the QEMU windows it spawns all depend on the invoking user's own session (see the comment at the top of `andlerd.service` for the full reasoning). Install with `scripts/install.sh` (run as yourself, not root).
