#!/usr/bin/env bash
# Removes the per-user andlerd systemd service installed by scripts/install.sh.
#
# Usage:
#   scripts/uninstall.sh              # stop + disable + remove the unit
#   scripts/uninstall.sh --purge      # also delete ~/.andler data and the
#                                     # /etc/sudoers.d/andler rules (asks on TTY)
#
# Deliberately scoped like install.sh: only touches the *current user's*
# systemd user directory (~/.config/systemd/user/). It does NOT delete the
# andlerd binary — install.sh never copies it, the unit points at an existing
# path — and it never touches instance data unless --purge is given. Run it
# as yourself, not with sudo.

set -euo pipefail

PURGE=0
if [[ $# -ge 1 ]]; then
    case "$1" in
        --purge) PURGE=1 ;;
        -h|--help)
            echo "usage: $0 [--purge]" >&2
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1 (only --purge is supported)" >&2
            exit 1
            ;;
    esac
fi

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
echo "Data (instances, database) left in place under ~/.andler —"
echo "run with --purge to delete it too."

if [[ "$PURGE" -eq 1 ]]; then
    echo
    echo "Purging data and system-wide privileged setup:"
    echo "  1. ~/.andler/  — all instances, disks, snapshots, cache"
    echo "  2. /etc/sudoers.d/andler  — passwordless-sudo rule added by 'andler doctor --fix'"
    echo "  3. /usr/local/sbin/andler-helper  — the privileged helper binary"
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

    if [[ -f /etc/sudoers.d/andler ]]; then
        echo "Removing /etc/sudoers.d/andler (interactive sudo)"
        if sudo rm -f /etc/sudoers.d/andler; then
            echo "Removed /etc/sudoers.d/andler"
        else
            echo "Warning: could not remove /etc/sudoers.d/andler — remove it manually:" >&2
            echo "  sudo rm /etc/sudoers.d/andler" >&2
        fi
    else
        echo "No /etc/sudoers.d/andler — nothing to remove"
    fi

    if [[ -f /usr/local/sbin/andler-helper ]]; then
        echo "Removing /usr/local/sbin/andler-helper (interactive sudo)"
        if sudo rm -f /usr/local/sbin/andler-helper; then
            echo "Removed /usr/local/sbin/andler-helper"
        else
            echo "Warning: could not remove /usr/local/sbin/andler-helper — remove it manually:" >&2
            echo "  sudo rm /usr/local/sbin/andler-helper" >&2
        fi
    else
        echo "No /usr/local/sbin/andler-helper — nothing to remove"
    fi

    echo
    echo "Purge complete."
fi
