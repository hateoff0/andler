#!/usr/bin/env bash
# Installs the ANDLER daemon as a per-user systemd service, and/or places the
# `andlerd` / `andler` binaries on your PATH — from a local build or straight
# from a published GitHub release.
#
# Usage:
#   scripts/install.sh [options] [path/to/andlerd]
#
# Options:
#   --component daemon|cli|both   what to install (default: daemon)
#   --from-release [TAG]          download from GitHub Releases instead of using
#                                 a local binary; TAG defaults to the latest
#                                 release (needs `gh`, or `curl` plus an
#                                 explicit TAG for a public repository)
#   --bin-dir DIR                 where binaries land (default: ~/.local/bin)
#   --no-service                  bins only: do not touch systemd
#   --repo OWNER/REPO             release source (default: hateoff0/andler)
#   -h, --help                    this text
#
# Run it as yourself, not with sudo: andlerd is a per-user service whose
# hardware detection and QEMU windows belong to your session (see the comment
# at the top of andlerd.service). It never creates users and never touches
# /etc/systemd/system.
#
# The CLI and the daemon must come from the same release: `andler` checks
# `GetVersion` before every command and refuses a daemon built from a different
# version, so installing one of them from a newer release than the other fails
# at the first command instead of misbehaving quietly.

set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/andlerd.service"

COMPONENT="daemon"
FROM_RELEASE=0
RELEASE_TAG=""
BIN_DIR="$HOME/.local/bin"
NO_SERVICE=0
REPO="${ANDLER_RELEASE_REPO:-hateoff0/andler}"
LOCAL_DAEMON=""

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --component)
            COMPONENT="${2:?--component needs daemon, cli or both}"
            shift 2
            ;;
        --from-release)
            FROM_RELEASE=1
            if [[ $# -ge 2 && "$2" != -* ]]; then
                RELEASE_TAG="$2"
                shift 2
            else
                shift
            fi
            ;;
        --bin-dir)
            BIN_DIR="${2:?--bin-dir needs a directory}"
            shift 2
            ;;
        --no-service)
            NO_SERVICE=1
            shift
            ;;
        --repo)
            REPO="${2:?--repo needs owner/repo}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        -*)
            echo "error: unknown option: $1" >&2
            echo "       run '$0 --help' for the list" >&2
            exit 1
            ;;
        *)
            LOCAL_DAEMON="$1"
            shift
            ;;
    esac
done

case "$COMPONENT" in
    daemon|cli|both) ;;
    *)
        echo "error: --component must be daemon, cli or both (got '$COMPONENT')" >&2
        exit 1
        ;;
esac

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    echo "error: run this as your normal user, not root/sudo — andlerd is a per-user service." >&2
    echo "       (see the comment at the top of scripts/andlerd.service for why)" >&2
    exit 1
fi

# --- which binaries, and where do they come from -----------------------------

wants_daemon=0
wants_cli=0
if [[ "$COMPONENT" == "daemon" || "$COMPONENT" == "both" ]]; then
    wants_daemon=1
fi
if [[ "$COMPONENT" == "cli" || "$COMPONENT" == "both" ]]; then
    wants_cli=1
fi

mkdir -p "$BIN_DIR"
if [[ ! -w "$BIN_DIR" ]]; then
    echo "error: $BIN_DIR is not writable." >&2
    echo "       pass --bin-dir with a directory you own (e.g. ~/.local/bin)" >&2
    exit 1
fi

scratch=""
cleanup() {
    # An EXIT trap's status becomes the script's status: keep this returning 0
    # when there is nothing to clean, or a successful install exits non-zero.
    if [[ -n "$scratch" ]]; then
        rm -rf "$scratch"
    fi
}
trap cleanup EXIT

if [[ "$FROM_RELEASE" -eq 1 ]]; then
    arch="$(uname -m)"
    if [[ "$arch" != "x86_64" ]]; then
        echo "error: published artifacts cover linux-x86_64 only (this host is $arch)." >&2
        echo "       build from source instead: cargo build --release -p daemon -p cli" >&2
        exit 1
    fi

    case "$COMPONENT" in
        both) prefix="andler" ;;
        daemon) prefix="andlerd" ;;
        cli) prefix="andler-cli" ;;
    esac

    if [[ -z "$RELEASE_TAG" ]]; then
        if ! command -v gh >/dev/null 2>&1; then
            echo "error: resolving the latest release needs the GitHub CLI." >&2
            echo "       either install 'gh', or pass the tag: $0 --from-release v0.1.0" >&2
            exit 1
        fi
        RELEASE_TAG="$(gh release view --repo "$REPO" --json tagName --jq .tagName)"
    fi

    asset="${prefix}-${RELEASE_TAG}-linux-x86_64.tar.gz"
    scratch="$(mktemp -d)"
    echo "Downloading $asset from $REPO"

    if command -v gh >/dev/null 2>&1; then
        if ! gh release download "$RELEASE_TAG" --repo "$REPO" --dir "$scratch" \
                --pattern "${asset}*"; then
            echo "error: ${asset} is not attached to release $RELEASE_TAG of $REPO." >&2
            echo "       what that release does publish:" >&2
            echo "       gh release view $RELEASE_TAG --repo $REPO" >&2
            exit 1
        fi
    else
        base="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"
        curl -fsSL -o "$scratch/$asset" "$base/$asset"
        curl -fsSL -o "$scratch/${asset}.sha256" "$base/${asset}.sha256"
    fi

    (cd "$scratch" && sha256sum -c "${asset}.sha256")
    tar -xzf "$scratch/$asset" -C "$scratch"
    src_dir="$scratch/${asset%.tar.gz}"

    if [[ "$wants_daemon" -eq 1 ]]; then
        install -m 0755 "$src_dir/andlerd" "$BIN_DIR/andlerd"
    fi
    if [[ "$wants_cli" -eq 1 ]]; then
        install -m 0755 "$src_dir/andler" "$BIN_DIR/andler"
    fi
else
    # Local install: the daemon comes from an explicit path, then PATH, then the
    # repository's own release build; the CLI is looked up the same way.
    if [[ "$wants_daemon" -eq 1 ]]; then
        if [[ -n "$LOCAL_DAEMON" ]]; then
            andlerd_src="$LOCAL_DAEMON"
        elif command -v andlerd >/dev/null 2>&1; then
            andlerd_src="$(command -v andlerd)"
        elif [[ -x "$SCRIPT_DIR/../target/release/andlerd" ]]; then
            andlerd_src="$SCRIPT_DIR/../target/release/andlerd"
        else
            echo "error: couldn't find an andlerd binary." >&2
            echo "       build one (cargo build --release -p daemon)," >&2
            echo "       pass its path, or use --from-release" >&2
            exit 1
        fi
        [[ -x "$andlerd_src" ]] || { echo "error: $andlerd_src is not executable." >&2; exit 1; }
        install -m 0755 "$andlerd_src" "$BIN_DIR/andlerd"
    fi
    if [[ "$wants_cli" -eq 1 ]]; then
        if command -v andler >/dev/null 2>&1; then
            andler_src="$(command -v andler)"
        elif [[ -x "$SCRIPT_DIR/../target/release/andler" ]]; then
            andler_src="$SCRIPT_DIR/../target/release/andler"
        else
            echo "error: couldn't find an andler binary." >&2
            echo "       build one (cargo build --release -p cli)," >&2
            echo "       or use --from-release" >&2
            exit 1
        fi
        install -m 0755 "$andler_src" "$BIN_DIR/andler"
    fi
fi

echo "Installed into $BIN_DIR:"
if [[ "$wants_daemon" -eq 1 ]]; then
    echo "  $("$BIN_DIR/andlerd" --version)  $(command -v -- "$BIN_DIR/andlerd")"
fi
if [[ "$wants_cli" -eq 1 ]]; then
    echo "  $("$BIN_DIR/andler" --version)  $(command -v -- "$BIN_DIR/andler")"
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo
        echo "note: $BIN_DIR is not on your PATH yet — add it, e.g.:"
        echo "      echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.bashrc"
        ;;
esac

# --- the service (daemon only) ----------------------------------------------

if [[ "$wants_daemon" -eq 0 || "$NO_SERVICE" -eq 1 ]]; then
    exit 0
fi

# systemd accepts an absolute path or a bare command name in ExecStart; a
# relative path yields a unit that fails to load, so resolve it here.
ANDLERD_BIN="$(readlink -f "$BIN_DIR/andlerd")"

echo
echo "Installing the andlerd user service"
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
