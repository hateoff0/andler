#!/usr/bin/env bash
# ANDLER uninstaller — reverses what scripts/install.sh installed.
#
# Scoped like the installer: only the *current user's* systemd user directory
# (~/.config/systemd/user/), the binaries install.sh placed in --bin-dir, and —
# with --purge — the data root. Instance data survives unless --purge is given,
# and an explicit 'yes' is required on a TTY (same policy as `andler remove
# --purge`); --yes answers for scripts. Run it as yourself, not with sudo.
#
# Flags and behavior are documented in scripts/README.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
UI_LIB="$SCRIPT_DIR/ui.sh"
if [[ ! -f "$UI_LIB" ]]; then
    printf 'error: %s is missing — run the uninstaller from a repository checkout (scripts/uninstall.sh).\n' "$UI_LIB" >&2
    exit 1
fi
# shellcheck source=scripts/ui.sh
source "$UI_LIB"

UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/andlerd.service"
DATA_DIR="${ANDLER_HOME:-$HOME/.andler}"

PURGE=0
BINARIES=0
ASSUME_YES=0
BIN_DIR="$HOME/.local/bin"

usage() {
    cat <<'EOF'
Remove the ANDLER user service, and — on request — the binaries and the data.

Usage:
  scripts/uninstall.sh [options]

Options:
  --binaries       also remove andler/andlerd from --bin-dir
  --bin-dir DIR    where the binaries live (default: ~/.local/bin; implies --binaries)
  --purge          also delete the data root (instances, disks, snapshots, database)
  --yes            answer 'yes' to the --purge confirmation (for scripts)
  --no-color       plain output (NO_COLOR is honoured too)
  -h, --help       this text

The data root is $ANDLER_HOME, defaulting to ~/.andler — --purge deletes that
directory and nothing else. Binaries elsewhere on PATH are never touched.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)
            PURGE=1
            shift
            ;;
        --binaries)
            BINARIES=1
            shift
            ;;
        --bin-dir)
            BIN_DIR="${2:?--bin-dir needs a directory}"
            BINARIES=1
            shift 2
            ;;
        --yes | -y)
            ASSUME_YES=1
            shift
            ;;
        --no-color)
            NO_COLOR=1
            export NO_COLOR
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            ui_init
            ui_error "unknown argument: $1"
            ui_hint "run '$0 --help' for the list"
            exit 1
            ;;
    esac
done

ui_init

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    ui_error "run this as your normal user, not root/sudo — the service is per-user"
    exit 1
fi

ui_banner "uninstaller" "Android Linux Emulator & Runtime"

# --- the user service --------------------------------------------------------

ui_section "User service"

if ! command -v systemctl >/dev/null 2>&1; then
    if [[ -f "$UNIT_DEST" ]]; then
        ui_warn "systemctl not available" "removing the unit file only"
    else
        ui_ok "nothing to do" "no systemd user instance, and no unit at $UNIT_DEST"
    fi
elif ! timeout 5 systemctl --user show-environment >/dev/null 2>&1; then
    ui_warn "systemd user instance" "not reachable — stopping/disabling is skipped"
    ui_fix "stop a daemon you started by hand: pkill -f andlerd"
else
    # Stop and disable first; failures here are real (a running daemon holds
    # instance locks and QEMU processes), so they are surfaced, not masked.
    if systemctl --user is-active --quiet andlerd 2>/dev/null; then
        systemctl --user stop andlerd
        ui_ok "stopped" "andlerd"
    else
        ui_ok "not running" "andlerd"
    fi
    if systemctl --user is-enabled --quiet andlerd 2>/dev/null; then
        systemctl --user disable andlerd >/dev/null
        ui_ok "disabled" "andlerd"
    fi
fi

if [[ -f "$UNIT_DEST" ]]; then
    rm -f "$UNIT_DEST"
    ui_ok "unit removed" "$UNIT_DEST"
    if command -v systemctl >/dev/null 2>&1 && timeout 5 systemctl --user show-environment >/dev/null 2>&1; then
        systemctl --user daemon-reload
    fi
else
    ui_ok "no unit file" "$UNIT_DEST"
fi

# --- binaries ----------------------------------------------------------------

ui_section "Binaries"

if [[ "$BINARIES" -eq 1 ]]; then
    removed=0
    for bin in andlerd andler; do
        if [[ -f "$BIN_DIR/$bin" ]]; then
            rm -f "$BIN_DIR/$bin"
            ui_ok "removed" "$BIN_DIR/$bin"
            removed=$((removed + 1))
        fi
    done
    if (( removed == 0 )); then
        ui_ok "nothing to do" "no andler binaries in $BIN_DIR"
    fi
else
    ui_ok "left in place" "$BIN_DIR/{andler,andlerd}"
    ui_fix "remove them with: $0 --binaries --bin-dir $BIN_DIR"
fi

# --- data --------------------------------------------------------------------

ui_section "Data"

if [[ "$PURGE" -eq 0 ]]; then
    if [[ -d "$DATA_DIR" ]]; then
        ui_ok "left in place" "$DATA_DIR  (instances, disks, snapshots, database)"
        ui_fix "delete it too with: $0 --purge"
    else
        ui_ok "nothing to do" "$DATA_DIR does not exist"
    fi
    printf '\n'
    ui_note "Uninstalled. Binaries elsewhere on PATH were never touched."
    printf '\n'
    exit 0
fi

if [[ ! -d "$DATA_DIR" ]]; then
    ui_ok "nothing to do" "$DATA_DIR does not exist"
    printf '\n'
    exit 0
fi

ui_warn "this deletes $DATA_DIR" "every instance, disk, snapshot and the SQLite store"

running_daemon="$(pgrep -f '(^|/)andlerd( |$)' 2>/dev/null | head -n 1 || true)"
if [[ -n "$running_daemon" ]]; then
    ui_warn "an andlerd process is still running" "pid $running_daemon"
    ui_fix "stop it first: systemctl --user stop andlerd, or kill $running_daemon"
fi

if [[ "$ASSUME_YES" -eq 1 ]]; then
    ui_ok "confirmation" "--yes was given"
elif [[ -t 0 ]]; then
    printf '      Type "yes" to delete %s: ' "$DATA_DIR"
    read -r answer
    if [[ "$answer" != "yes" ]]; then
        ui_error "aborted — nothing was deleted"
        exit 1
    fi
else
    ui_error "refusing to delete $DATA_DIR without a confirmation"
    ui_hint "run it on a terminal, or pass --yes for a script"
    exit 1
fi

rm -rf "$DATA_DIR"
ui_ok "removed" "$DATA_DIR"

runtime_dir="${XDG_RUNTIME_DIR:-}/andler"
if [[ -n "${XDG_RUNTIME_DIR:-}" && -d "$runtime_dir" ]]; then
    if [[ -n "$running_daemon" ]]; then
        ui_warn "kept $runtime_dir" "an andlerd process is still using its sockets"
    else
        rm -rf "$runtime_dir"
        ui_ok "removed" "$runtime_dir  (QMP/QGA sockets)"
    fi
fi

printf '\n'
ui_note "Purge complete."
printf '\n'
