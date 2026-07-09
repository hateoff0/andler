#!/usr/bin/env bash
# Installs andlerd as a per-user systemd service (see andlerd.service in
# this directory for why it's a user unit, not a system one, and
# PLAN.md, item 9, "Daemon systemd integration").
#
# Usage:
#   scripts/install.sh                 # auto-detect andlerd binary
#   scripts/install.sh /path/to/andlerd  # use an explicit binary path
#
# Does NOT create a system user/group and does NOT touch
# /etc/systemd/system — this only ever installs into the *current
# user's* systemd user directory (~/.config/systemd/user/), matching
# how andlerd resolves ANDLER_HOME (per-user by default, see
# core/andler-core/src/paths.rs). Run it as yourself, not with sudo.

set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/andlerd.service"

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    echo "error: run this as your normal user, not root/sudo — andlerd is a per-user service." >&2
    echo "       (see the comment at the top of scripts/andlerd.service for why)" >&2
    exit 1
fi

# Resolve the andlerd binary: explicit arg > PATH > local release build.
if [[ $# -ge 1 ]]; then
    ANDLERD_BIN="$1"
elif command -v andlerd >/dev/null 2>&1; then
    ANDLERD_BIN="$(command -v andlerd)"
elif [[ -x "$REPO_ROOT/target/release/andlerd" ]]; then
    ANDLERD_BIN="$REPO_ROOT/target/release/andlerd"
else
    echo "error: couldn't find an andlerd binary." >&2
    echo "       build one first (cargo build --release -p daemon)," >&2
    echo "       or pass its path explicitly: scripts/install.sh /path/to/andlerd" >&2
    exit 1
fi

if [[ ! -x "$ANDLERD_BIN" ]]; then
    echo "error: $ANDLERD_BIN is not an executable file." >&2
    exit 1
fi

echo "Installing andlerd user service"
echo "  binary: $ANDLERD_BIN"
echo "  unit:   $UNIT_DEST"

mkdir -p "$UNIT_DIR"
sed "s|^ExecStart=andlerd\$|ExecStart=$ANDLERD_BIN|" \
    "$SCRIPT_DIR/andlerd.service" > "$UNIT_DEST"

systemctl --user daemon-reload
systemctl --user enable --now andlerd

echo
echo "Done. Check status with: systemctl --user status andlerd"
echo "Logs:                    journalctl --user -u andlerd -f"
