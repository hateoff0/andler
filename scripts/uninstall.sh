#!/usr/bin/env bash
# Removes the per-user andlerd systemd service installed by scripts/install.sh.
#
# Usage:
#   scripts/uninstall.sh                    # stop + disable + remove the unit
#   scripts/uninstall.sh --binaries         # also remove ~/.local/bin/{andler,andlerd}
#   scripts/uninstall.sh --purge            # also delete ~/.andler data (asks on TTY)
#
# Deliberately scoped like install.sh: only touches the *current user's*
# systemd user directory (~/.config/systemd/user/) and the binaries install.sh
# placed in --bin-dir (default ~/.local/bin, only when --binaries is given).
# Instance data is untouched unless --purge is passed. Run it as yourself, not
# with sudo.

set -euo pipefail

PURGE=0
BINARIES=0
BIN_DIR="$HOME/.local/bin"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge) PURGE=1; shift ;;
        --binaries) BINARIES=1; shift ;;
        --bin-dir)
            BIN_DIR="${2:?--bin-dir needs a directory}"
            BINARIES=1
            shift 2
            ;;
        -h|--help)
            echo "usage: $0 [--purge] [--binaries] [--bin-dir DIR]" >&2
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1 (see --help)" >&2
            exit 1
            ;;
    esac
done

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    echo "error: run this as your normal user, not root/sudo — the service is per-user." >&2
    exit 1
fi

UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/andlerd.service"

# Stop and disable first; failures here are real (a running daemon holds
# instance locks and QEMU processes), so do not mask them.
if systemctl --user is-active --quiet andlerd 2>/dev/null; then
    echo "Stopping andlerd service"
    systemctl --user stop andlerd
fi
if systemctl --user is-enabled --quiet andlerd 2>/dev/null; then
    echo "Disabling andlerd service"
    systemctl --user disable andlerd
fi

if [[ -f "$UNIT_DEST" ]]; then
    echo "Removing $UNIT_DEST"
    rm -f "$UNIT_DEST"
else
    echo "No unit at $UNIT_DEST — nothing to remove"
fi

systemctl --user daemon-reload

echo "Uninstalled andlerd user service."

if [[ "$BINARIES" -eq 1 ]]; then
    for bin in andlerd andler; do
        if [[ -f "$BIN_DIR/$bin" ]]; then
            rm -f "$BIN_DIR/$bin"
            echo "Removed $BIN_DIR/$bin"
        fi
    done
else
    echo "Binaries in $BIN_DIR (and anywhere else on PATH) were left in place —"
    echo "run with --binaries to remove the ones install.sh put in $BIN_DIR."
fi

echo "Data (instances, database) left in place under ~/.andler —"
echo "run with --purge to delete it too."

if [[ "$PURGE" -eq 1 ]]; then
    echo
    echo "Purging data:"
    echo "  ~/.andler/  — all instances, disks, snapshots, cache"
    echo

    # Deleting instance disks is irreversible; require an explicit
    # confirmation on a TTY (same policy as `andler remove --purge`).
    if [[ -t 0 ]]; then
        read -r -p "Type 'yes' to delete all ANDLER data: " answer
    else
        answer=""
    fi
    if [[ "$answer" != "yes" ]]; then
        echo "Aborted — nothing was deleted." >&2
        exit 1
    fi

    if [[ -d "$HOME/.andler" ]]; then
        echo "Removing $HOME/.andler"
        rm -rf "$HOME/.andler"
    else
        echo "No $HOME/.andler — nothing to remove"
    fi

    echo
    echo "Purge complete."
fi
