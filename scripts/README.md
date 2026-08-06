# scripts

Helper development scripts, not part of the product.

## `andlerd.service` / `install.sh` / `uninstall.sh`

Systemd **user** service for `andlerd` (see ARCHITECTURE.md, item 9). Deliberately a user unit (`systemctl --user`), not a system unit — `andlerd`'s GPU/audio/display auto-detection and the QEMU windows it spawns all depend on the invoking user's own session (see the comment at the top of `andlerd.service` for the full reasoning). Install with `scripts/install.sh` (run as yourself, not root); remove with `scripts/uninstall.sh`. `uninstall.sh --purge` additionally deletes `~/.andler` (all instances/disks/snapshots — asks for confirmation) and the `/etc/sudoers.d/andler` rules added by `andler doctor --fix`; it never deletes the andlerd binary.
