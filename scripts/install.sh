#!/usr/bin/env bash
# ANDLER installer — host dependency report, the `andler` + `andlerd` binaries,
# and the per-user systemd service for the daemon.
#
# Run it as yourself, not with sudo: andlerd is a *user* service whose hardware
# detection and QEMU windows belong to your session (see the comment at the top
# of scripts/andlerd.service). It never creates users and never touches
# /etc/systemd/system.
#
# The CLI and the daemon must come from the same release: `andler` checks the
# daemon's version before every command and refuses a daemon built from a
# different one. Installing both (the default) is the path that cannot drift.
#
# Flags and behavior are documented in scripts/README.md; uninstall.sh reverses
# what this script installs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
UI_LIB="$SCRIPT_DIR/ui.sh"
DEPS_LIB="$SCRIPT_DIR/deps.sh"
for lib in "$UI_LIB" "$DEPS_LIB"; do
    if [[ ! -f "$lib" ]]; then
        printf 'error: %s is missing — run the installer from a repository checkout (scripts/install.sh).\n' "$lib" >&2
        exit 1
    fi
done
# shellcheck source=scripts/ui.sh
source "$UI_LIB"
# shellcheck source=scripts/deps.sh
source "$DEPS_LIB"

UNIT_TEMPLATE="$SCRIPT_DIR/andlerd.service"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/andlerd.service"

# Mirrors andler-firmware's discovery (services/andler-firmware/src/detect/ovmf.rs),
# one distro per position, so the installer's verdict matches `andler doctor`'s.
OVMF_CODE_PATHS=(
    /usr/share/edk2/x64/OVMF_CODE.4m.fd
    /usr/share/OVMF/OVMF_CODE_4M.fd
    /usr/share/qemu/ovmf-x86_64-code.bin
    /usr/share/edk2/ovmf/OVMF_CODE.fd
    /usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd
)
OVMF_VARS_PATHS=(
    /usr/share/edk2/x64/OVMF_VARS.4m.fd
    /usr/share/OVMF/OVMF_VARS_4M.fd
    /usr/share/qemu/ovmf-x86_64-vars.bin
    /usr/share/edk2/ovmf/OVMF_VARS.fd
    /usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd
)

# key|kind|target|scope|level|why|condition
#
#   kind       bin (one binary), bin-any (any of the listed), kvm, ovmf,
#              systemd-user, userns, tun, cap
#   scope      daemon | cli | service | installer | all
#   level      required (a missing one stops the install) | optional
#   condition  release (only with --from-release), local (only without),
#              nvidia (only when an NVIDIA GPU is present), empty = always
DEP_TABLE=(
    'qemu|bin|qemu-system-x86_64|daemon|required|spawn and run QEMU/KVM virtual machines|'
    'qemu-img|bin|qemu-img|daemon|required|create, clone, resize, compact and snapshot disks|'
    'ovmf|ovmf|-|daemon|required|the UEFI firmware every guest boots from|'
    'kvm|kvm|/dev/kvm|daemon|required|hardware virtualization (KVM access)|'
    'tar-release|bin|tar|installer|required|unpack the release archive|release'
    'sha256-release|bin|sha256sum|installer|required|verify the release checksum|release'
    'download|bin-any|curl gh|installer|required|download the release assets|release'
    'systemd|systemd-user|-|service|required|run andlerd as a systemd user service|'
    'tar|bin|tar|daemon|optional|packing ARM-translator payloads|local'
    'ip|bin|ip|daemon|optional|Bridge and Isolated networking|'
    'unshare|bin|unshare|daemon|optional|Isolated networking (network namespace)|'
    'userns|userns|-|daemon|optional|Isolated networking, and the libguestfs appliance|'
    'tun|tun|/dev/net/tun|daemon|optional|guest tap devices for Bridge/Isolated networking|'
    'cap-net-admin|cap|-|daemon|optional|Bridge networking|'
    'passt|bin|passt|daemon|optional|NAT through passt instead of slirp|'
    'guestfish|bin|guestfish|daemon|optional|offline guest operations (install/remove, boot mode, ARM translators)|'
    'debugfs|bin|debugfs|daemon|optional|reading build.prop out of the Waydroid system image|'
    'oras|bin|oras|daemon|optional|pushing an exported OCI layout to a registry|'
    'lspci|bin|lspci|daemon|optional|GPU vendor auto-detection outside /sys/class/drm|'
    'glxinfo|bin|glxinfo|daemon|optional|host Mesa version for the Venus capability check|'
    'nvidia-smi|bin|nvidia-smi|daemon|optional|NVIDIA GPU metrics (VRAM, utilisation)|nvidia'
)

COMPONENT="both"
FROM_RELEASE=0
RELEASE_TAG=""
BIN_DIR="$HOME/.local/bin"
NO_SERVICE=0
REPO="${ANDLER_RELEASE_REPO:-hateoff0/andler}"
LOCAL_DAEMON=""
CHECK_DEPS=0
SKIP_DEPS=0
WITH_OPTIONAL=0
DRY_RUN=0
DATA_DIR="${ANDLER_HOME:-$HOME/.andler}"
OPTIONAL_RECORD="$DATA_DIR/optional-packages"

usage() {
    cat <<'EOF'
Install ANDLER: host dependency report, the daemon + CLI binaries, and the
per-user systemd service for the daemon.

Usage:
  scripts/install.sh [options] [path/to/andlerd]

Options:
  --component daemon|cli|both  what to install (default: both)
  --from-release [TAG]         install a published GitHub release instead of
                               local binaries; TAG defaults to the latest
                               release (uses `gh` when present, otherwise
                               `curl` against the GitHub API)
  --bin-dir DIR                where the binaries land (default: ~/.local/bin)
  --no-service                 do not touch systemd (binaries only)
  --with-optional              also install the optional dependencies the report
                               lists as missing (iproute2, util-linux, passt,
                               guestfs, e2fsprogs, pciutils, mesa-utils) through
                               the detected package manager; what was installed
                               is recorded so `uninstall.sh --optional` removes
                               exactly that set again
  --check-deps                 report host dependencies and exit
  --skip-deps                  install without checking host dependencies
  --dry-run                    print the plan (and the optional package
                               command) and change nothing
  --repo OWNER/REPO            release source (default: hateoff0/andler)
  --no-color                   plain output (NO_COLOR is honoured too)
  -h, --help                   this text

Without --from-release the binaries come from a path you pass, from PATH, or
from target/release of a checkout built with cargo.

Examples:
  scripts/install.sh                              # local build: daemon + CLI + service
  scripts/install.sh --from-release               # latest release, same
  scripts/install.sh --check-deps                 # host dependency report only
  scripts/install.sh --component cli --no-service --from-release v0.1.0
EOF
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
        --check-deps)
            CHECK_DEPS=1
            shift
            ;;
        --skip-deps)
            SKIP_DEPS=1
            shift
            ;;
        --with-optional | --optional)
            WITH_OPTIONAL=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --no-color)
            NO_COLOR=1
            export NO_COLOR
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
            ui_init
            ui_error "unknown option: $1"
            ui_hint "run '$0 --help' for the list"
            exit 1
            ;;
        *)
            LOCAL_DAEMON="$1"
            shift
            ;;
    esac
done

ui_init

case "$COMPONENT" in
    daemon|cli|both) ;;
    *)
        ui_error "--component must be daemon, cli or both (got '$COMPONENT')"
        exit 1
        ;;
esac

if [[ "$CHECK_DEPS" -eq 1 && "$SKIP_DEPS" -eq 1 ]]; then
    ui_error "--check-deps and --skip-deps contradict each other"
    exit 1
fi

if [[ "$WITH_OPTIONAL" -eq 1 && "$SKIP_DEPS" -eq 1 ]]; then
    ui_error "--with-optional has nothing to install without the dependency check"
    ui_hint "--skip-deps means 'do not look at the host'; drop one of the two"
    exit 1
fi

if [[ "$WITH_OPTIONAL" -eq 1 && "$CHECK_DEPS" -eq 1 ]]; then
    ui_error "--check-deps reports and exits — it installs no packages"
    ui_hint "run --with-optional on its own, or keep --check-deps to see the report first"
    exit 1
fi

# --check-deps and --dry-run change nothing, so they are safe to run as root
# (containers and CI images usually are). Installing is not: andlerd is a
# per-user service whose unit, binaries and data belong to the invoking user.
read_only_run=0
if [[ "$CHECK_DEPS" -eq 1 || "$DRY_RUN" -eq 1 ]]; then
    read_only_run=1
fi

if [[ "${EUID:-$(id -u)}" -eq 0 && "$read_only_run" -eq 0 ]]; then
    ui_error "run this as your normal user, not root/sudo — andlerd is a per-user service"
    ui_hint "why: the comment at the top of scripts/andlerd.service"
    ui_hint "a read-only run is fine as root: --check-deps, --dry-run"
    exit 1
fi

wants_daemon=0
wants_cli=0
if [[ "$COMPONENT" == "daemon" || "$COMPONENT" == "both" ]]; then
    wants_daemon=1
fi
if [[ "$COMPONENT" == "cli" || "$COMPONENT" == "both" ]]; then
    wants_cli=1
fi
wants_service=0
if [[ "$wants_daemon" -eq 1 && "$NO_SERVICE" -eq 0 ]]; then
    wants_service=1
fi

# --- distro family, for install commands -------------------------------------

FAMILY="$(deps_detect_family)"

# How a dependency is named in the report — the binary or the thing that breaks.
dep_label() {
    case "$1" in
        qemu) printf 'qemu-system-x86_64' ;;
        qemu-img) printf 'qemu-img' ;;
        ovmf) printf 'OVMF/UEFI firmware' ;;
        kvm) printf '/dev/kvm' ;;
        tar-release | tar) printf 'tar' ;;
        sha256-release) printf 'sha256sum' ;;
        download) printf 'curl (or gh)' ;;
        systemd) printf 'systemd --user' ;;
        ip) printf 'ip (iproute2)' ;;
        unshare) printf 'unshare (util-linux)' ;;
        userns) printf 'unprivileged user namespaces' ;;
        tun) printf '/dev/net/tun' ;;
        cap-net-admin) printf 'CAP_NET_ADMIN' ;;
        passt) printf 'passt' ;;
        guestfish) printf 'guestfish (guestfs-tools)' ;;
        debugfs) printf 'debugfs (e2fsprogs)' ;;
        oras) printf 'oras' ;;
        lspci) printf 'lspci (pciutils)' ;;
        glxinfo) printf 'glxinfo (mesa-utils)' ;;
        nvidia-smi) printf 'nvidia-smi' ;;
        *) printf '%s' "$1" ;;
    esac
}

# One actionable line for a dependency the host is missing.
fix_for() {
    local key="$1" pkgs
    case "$key" in
        kvm)
            printf 'sudo usermod -aG kvm $USER   (then log out and back in)'
            return
            ;;
        cap-net-admin)
            printf 'sudo setcap cap_net_admin+ep "$(command -v andlerd)"   — or use network mode Nat/Isolated, which need no capability'
            return
            ;;
        userns)
            printf 'unprivileged user namespaces are disabled: sudo sysctl -w user.max_user_namespaces=10000 (andler doctor prints the kernel-specific fix)'
            return
            ;;
        tun)
            printf 'sudo modprobe tun   (the tun module provides /dev/net/tun)'
            return
            ;;
        oras)
            printf 'install oras from https://oras.land (Arch: AUR package oras)'
            return
            ;;
        systemd)
            printf 'install systemd, or start a user session bus, then retry — or pass --no-service'
            return
            ;;
    esac

    pkgs="$(pkg_names_for_family "$key" "$FAMILY")"
    if [[ "$FAMILY" == "unknown" || -z "$pkgs" || "$pkgs" == "-" ]]; then
        printf 'install the package that provides %s (%s)' "$(dep_label "$key")" "$pkgs"
        return
    fi
    printf '%s' "$(pkg_install_command "$FAMILY" "$pkgs")"
}

# --- individual probes -------------------------------------------------------

nvidia_present() {
    local dev
    for dev in /sys/bus/pci/devices/*/vendor; do
        [[ -r "$dev" ]] || continue
        if [[ "$(cat "$dev")" == "0x10de" ]]; then
            return 0
        fi
    done
    return 1
}

ovmf_pair() {
    if [[ -n "${ANDLERD_OVMF_CODE:-}" && -n "${ANDLERD_OVMF_VARS:-}" ]] &&
        [[ -e "$ANDLERD_OVMF_CODE" && -e "$ANDLERD_OVMF_VARS" ]]; then
        printf '%s + %s (from ANDLERD_OVMF_CODE / ANDLERD_OVMF_VARS)' "$ANDLERD_OVMF_CODE" "$ANDLERD_OVMF_VARS"
        return 0
    fi
    local i code vars
    for i in "${!OVMF_CODE_PATHS[@]}"; do
        if [[ -e "${OVMF_CODE_PATHS[$i]}" && -e "${OVMF_VARS_PATHS[$i]}" ]]; then
            printf '%s + %s' "${OVMF_CODE_PATHS[$i]}" "${OVMF_VARS_PATHS[$i]}"
            return 0
        fi
    done
    for code in "${OVMF_CODE_PATHS[@]}"; do
        [[ -e "$code" ]] || continue
        for vars in "${OVMF_VARS_PATHS[@]}"; do
            if [[ -e "$vars" ]]; then
                printf '%s + %s' "$code" "$vars"
                return 0
            fi
        done
    done
    return 1
}

cap_net_admin_held() {
    local hex num
    hex="$(sed -n 's/^CapEff:[[:space:]]*\([0-9a-fA-F]*\).*/\1/p' /proc/self/status 2>/dev/null | head -n 1)"
    [[ -n "$hex" ]] || return 1
    num=$((16#$hex))
    (( num & (1 << 12) )) || return 1
    return 0
}

systemd_user_ready() {
    command -v systemctl >/dev/null 2>&1 || return 1
    timeout 5 systemctl --user show-environment >/dev/null 2>&1 || return 1
    return 0
}

userns_allowed() {
    command -v unshare >/dev/null 2>&1 || return 1
    timeout 5 unshare --user --map-root-user true >/dev/null 2>&1 || return 1
    return 0
}

dep_applies() {
    local scope="$1" cond="$2"
    case "$cond" in
        release) [[ "$FROM_RELEASE" -eq 1 ]] || return 1 ;;
        local) [[ "$FROM_RELEASE" -eq 0 ]] || return 1 ;;
        nvidia) nvidia_present || return 1 ;;
    esac
    case "$scope" in
        all) return 0 ;;
        daemon) [[ "$wants_daemon" -eq 1 ]] ;;
        cli) [[ "$wants_cli" -eq 1 ]] ;;
        service) [[ "$wants_service" -eq 1 ]] ;;
        installer) [[ "$FROM_RELEASE" -eq 1 ]] ;;
        *) return 0 ;;
    esac
}

# Prints the one-line detail for an applicable dependency; status 1 = missing.
dep_detail() {
    local kind="$1" target="$2" first detail
    case "$kind" in
        bin)
            detail="$(command -v "$target" 2>/dev/null || true)"
            [[ -n "$detail" ]] || return 1
            printf '%s' "$detail"
            ;;
        bin-any)
            for first in $target; do
                if detail="$(command -v "$first" 2>/dev/null)"; then
                    printf '%s' "$detail"
                    return 0
                fi
            done
            return 1
            ;;
        kvm)
            if [[ -e "$target" && -r "$target" && -w "$target" ]]; then
                printf 'accessible'
            elif [[ -e "$target" ]]; then
                printf 'present but not accessible'
                return 1
            else
                printf 'not found — no KVM on this machine'
                return 1
            fi
            ;;
        ovmf)
            ovmf_pair || return 1
            ;;
        userns)
            if userns_allowed; then
                printf 'allowed'
            else
                printf 'refused by the kernel'
                return 1
            fi
            ;;
        tun)
            if [[ -e "$target" ]]; then
                printf 'present'
            else
                printf 'missing — no tun module or no /dev/net/tun'
                return 1
            fi
            ;;
        cap)
            if cap_net_admin_held; then
                printf 'held'
            else
                printf 'not held'
                return 1
            fi
            ;;
        systemd-user)
            if systemd_user_ready; then
                printf 'systemd --user reachable'
            elif command -v systemctl >/dev/null 2>&1; then
                printf 'systemctl present, but no systemd user instance answers'
                return 1
            else
                printf 'systemctl not found'
                return 1
            fi
            ;;
    esac
}

# --- the dependency report ---------------------------------------------------

MISSING_REQUIRED=()
MISSING_PKGS=()
MISSING_OPTIONAL_KEYS=()
ABSENT_OPTIONAL=0

# Optional dependencies `--with-optional` will not install on its own: the
# NVIDIA driver is a kernel-module package that wants a reboot and a
# distribution-specific flavour, and oras is not packaged everywhere.
OPTIONAL_NOT_AUTO_INSTALLED=("nvidia-smi" "oras")

report_dependency() {
    local key="$1" kind="$2" target="$3" level="$4" why="$5" detail pkgs

    if detail="$(dep_detail "$kind" "$target")"; then
        ui_ok "$(dep_label "$key")" "$detail"
        return 0
    fi

    if [[ "$level" == "required" ]]; then
        ui_fail "$(dep_label "$key")" "$why"
        ui_fix "$(fix_for "$key")"
        MISSING_REQUIRED+=("$(dep_label "$key")")
        pkgs="$(pkg_names_for_family "$key" "$FAMILY")"
        if [[ "$FAMILY" != "unknown" && -n "$pkgs" && "$pkgs" != "-" ]]; then
            MISSING_PKGS+=("$pkgs")
        fi
    else
        ABSENT_OPTIONAL=$((ABSENT_OPTIONAL + 1))
        MISSING_OPTIONAL_KEYS+=("$key")
        ui_warn "$(dep_label "$key")" "$why"
        ui_fix "$(fix_for "$key")"
    fi
    return 0
}

# Packages `--with-optional` can install for the missing decorative set, one
# package per key, deduplicated (util-linux and e2fsprogs are usually present
# already, so the list is nearly always shorter than the report). Empty when the
# distribution is unknown: there is no package manager to name them from.
optional_packages_to_install() {
    local key pkg seen=() seen_pkg out=() skip
    [[ "$FAMILY" == "unknown" ]] && return 0
    for key in ${MISSING_OPTIONAL_KEYS[@]+"${MISSING_OPTIONAL_KEYS[@]}"}; do
        skip=0
        for seen in ${OPTIONAL_NOT_AUTO_INSTALLED[@]+"${OPTIONAL_NOT_AUTO_INSTALLED[@]}"}; do
            [[ "$key" == "$seen" ]] && skip=1
        done
        (( skip )) && continue
        pkg="$(pkg_names_for_family "$key" "$FAMILY")"
        [[ -n "$pkg" && "$pkg" != "-" ]] || continue
        for seen_pkg in ${out[@]+"${out[@]}"}; do
            [[ "$pkg" == "$seen_pkg" ]] && skip=1
        done
        (( skip )) && continue
        out+=("$pkg")
    done
    printf '%s' "${out[*]:-}"
}

# The missing optional dependencies as the report names them — what to say when
# there is no package manager to build a command for.
missing_optional_labels() {
    local key out=""
    for key in ${MISSING_OPTIONAL_KEYS[@]+"${MISSING_OPTIONAL_KEYS[@]}"}; do
        out+="${out:+, }$(dep_label "$key")"
    done
    printf '%s' "$out"
}

# Installs the missing optional packages this host can get from its own
# package manager, and records them so `uninstall.sh --optional` removes
# exactly that set again. Prints the plan and stops under --dry-run.
install_optional_dependencies() {
    local pkgs cmd

    if [[ "$FAMILY" == "unknown" ]]; then
        if (( ${#MISSING_OPTIONAL_KEYS[@]} == 0 )); then
            ui_ok "nothing to install" "no optional dependency is missing"
            return 0
        fi
        ui_fail "unknown distribution" "no package manager to drive"
        ui_fix "install what the report listed as missing: $(missing_optional_labels)"
        return 1
    fi

    pkgs="$(optional_packages_to_install)"
    if [[ -z "$pkgs" ]]; then
        ui_ok "nothing to install" "every optional dependency this flag covers is already present"
        return 0
    fi

    cmd="$(pkg_install_command "$FAMILY" "$pkgs")"
    ui_kv "packages" "$pkgs"
    ui_kv "command" "$cmd"
    if [[ "$DRY_RUN" -eq 1 ]]; then
        ui_note "--dry-run: nothing was installed"
        return 0
    fi
    if ! command -v sudo >/dev/null 2>&1; then
        ui_fail "sudo" "not found — system packages need it"
        ui_fix "install them yourself: ${cmd#sudo }"
        return 1
    fi

    if ! bash -c "$cmd"; then
        ui_error "the package manager did not complete"
        ui_hint "run it yourself for the full output: $cmd"
        return 1
    fi
    ui_ok "installed" "$pkgs"
    record_optional_packages "$pkgs"
}

# Records the set in $DATA_DIR/optional-packages: a comment header saying what
# wrote it and how to undo it, the package manager family it was installed
# with (removal uses that, not a fresh detection), then one package per line —
# sorted and unique, merged with whatever an earlier --with-optional left.
record_optional_packages() {
    local existing=""
    if [[ -f "$OPTIONAL_RECORD" ]]; then
        existing="$(sed '/^#/d;/^family=/d' "$OPTIONAL_RECORD")"
    fi
    mkdir -p "$DATA_DIR"
    {
        printf '# ANDLER optional packages — installed by scripts/install.sh --with-optional\n'
        printf '# remove the set with: scripts/uninstall.sh --optional\n'
        printf 'family=%s\n' "$FAMILY"
        {
            printf '%s\n' "$existing" | tr ' ' '\n'
            printf '%s\n' "$1" | tr ' ' '\n'
        } | sed '/^$/d' | sort -u
    } >"$OPTIONAL_RECORD"
    ui_ok "recorded" "$OPTIONAL_RECORD (uninstall.sh --optional removes this set)"
}

check_dependencies() {
    local row key kind target scope level why cond row_level checked=0

    ui_section "Host dependencies"

    for row_level in required optional; do
        local printed_header=0
        for row in "${DEP_TABLE[@]}"; do
            IFS='|' read -r key kind target scope level why cond <<<"$row"
            [[ "$level" == "$row_level" ]] || continue
            dep_applies "$scope" "$cond" || continue
            checked=$((checked + 1))
            if (( printed_header == 0 )); then
                if [[ "$row_level" == "required" ]]; then
                    ui_note "required for '--component $COMPONENT'$([[ "$wants_service" -eq 1 ]] && echo " + the systemd user service")"
                else
                    printf '\n'
                    ui_note "optional — a missing one disables only the feature named, everything else works"
                fi
                printed_header=1
            fi
            report_dependency "$key" "$kind" "$target" "$level" "$why"
        done
    done

    if (( checked == 0 )); then
        ui_note "nothing to check on this host for '--component $COMPONENT' — a client machine runs no VMs"
    fi

    printf '\n'
    if (( ${#MISSING_REQUIRED[@]} == 0 )); then
        ui_ok "required dependencies" "all present"
        if (( ABSENT_OPTIONAL == 1 )); then
            ui_note "1 optional dependency is missing — the feature named above stays unavailable"
        elif (( ABSENT_OPTIONAL > 1 )); then
            ui_note "$ABSENT_OPTIONAL optional dependencies are missing — the features named above stay unavailable"
        fi
        if [[ "$WITH_OPTIONAL" -eq 0 && -n "$(optional_packages_to_install)" ]]; then
            ui_note "install them through your package manager with: $0 --with-optional"
        fi
        return 0
    fi

    ui_fail "${#MISSING_REQUIRED[@]} required dependencies missing" "${MISSING_REQUIRED[*]}"
    if (( ${#MISSING_PKGS[@]} > 0 )); then
        local names cmd
        names="$(printf '%s\n' "${MISSING_PKGS[@]}" | tr ' ' '\n' | sed '/^$/d' | sort -u | tr '\n' ' ')"
        case "$FAMILY" in
            arch) cmd="sudo pacman -S --needed ${names% }" ;;
            debian) cmd="sudo apt install ${names% }" ;;
            fedora) cmd="sudo dnf install ${names% }" ;;
            *) cmd="" ;;
        esac
        if [[ -n "$cmd" ]]; then
            ui_fix "install them all: $cmd"
        fi
    fi
    ui_fix "'andler doctor' explains the same checks in more depth once the daemon is installed"
    return 0
}

# --- release download --------------------------------------------------------

resolve_latest_tag() {
    local api="https://api.github.com/repos/${REPO}/releases/latest" json=""
    local -a auth=()
    local token="${ANDLER_INSTALL_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
    if [[ -n "$token" ]]; then
        auth=(-H "Authorization: Bearer $token")
    fi

    if command -v gh >/dev/null 2>&1; then
        if gh release view --repo "$REPO" --json tagName --jq .tagName 2>/dev/null; then
            return 0
        fi
        ui_note "gh could not read $REPO — falling back to the GitHub API"
    fi
    if ! command -v curl >/dev/null 2>&1; then
        ui_error "resolving the latest release needs gh or curl"
        ui_hint "install one of them, or pass the tag: $0 --from-release v0.1.0"
        exit 1
    fi

    if ! json="$(curl -fsSL --connect-timeout 10 --max-time 60 "${auth[@]}" \
        -H 'Accept: application/vnd.github+json' "$api")"; then
        ui_error "cannot read the latest release of $REPO"
        ui_hint "check the network and the repository name, pass a tag (--from-release v0.1.0), or set GH_TOKEN for a private repository"
        exit 1
    fi
    printf '%s' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

download_release() {
    local base
    step "Download"
    ui_kv "release" "$RELEASE_TAG ($REPO)"
    ui_kv "asset" "$asset"

    if command -v gh >/dev/null 2>&1; then
        if ! gh release download "$RELEASE_TAG" --repo "$REPO" --dir "$scratch" \
            --pattern "${asset}*"; then
            ui_error "${asset} is not attached to release $RELEASE_TAG of $REPO"
            ui_hint "what that release publishes: gh release view $RELEASE_TAG --repo $REPO"
            exit 1
        fi
    else
        base="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"
        if ! curl -fsSL --connect-timeout 10 --max-time 600 -o "$scratch/$asset" "$base/$asset"; then
            ui_error "cannot download $base/$asset"
            ui_hint "check the tag and the network, or install gh: https://cli.github.com"
            exit 1
        fi
        if ! curl -fsSL --connect-timeout 10 --max-time 60 -o "$scratch/${asset}.sha256" "$base/${asset}.sha256"; then
            ui_error "cannot download ${asset}.sha256"
            exit 1
        fi
    fi
    ui_ok "downloaded" "$asset"

    if ! (cd "$scratch" && sha256sum -c "${asset}.sha256") >/dev/null; then
        ui_error "${asset} failed its checksum check — refusing to install it"
        exit 1
    fi
    ui_ok "checksum verified" "${asset}.sha256"

    tar -xzf "$scratch/$asset" -C "$scratch"
    ui_ok "unpacked" "${asset%.tar.gz}"
}

# --- local binaries ----------------------------------------------------------

resolve_local_daemon() {
    if [[ -n "$LOCAL_DAEMON" ]]; then
        printf '%s' "$LOCAL_DAEMON"
        return 0
    fi
    if command -v andlerd >/dev/null 2>&1; then
        command -v andlerd
        return 0
    fi
    if [[ -x "$SCRIPT_DIR/../target/release/andlerd" ]]; then
        printf '%s' "$SCRIPT_DIR/../target/release/andlerd"
        return 0
    fi
    return 1
}

resolve_local_cli() {
    if command -v andler >/dev/null 2>&1; then
        command -v andler
        return 0
    fi
    if [[ -x "$SCRIPT_DIR/../target/release/andler" ]]; then
        printf '%s' "$SCRIPT_DIR/../target/release/andler"
        return 0
    fi
    return 1
}

# --- plan and install --------------------------------------------------------

# The post-install `andler doctor` pass needs the CLI and a daemon on *this*
# host: doctor reports on the machine it runs on, so asking it about a daemon
# elsewhere would describe the wrong hardware.
verify_eligible=0
if [[ "$wants_cli" -eq 1 && "$DRY_RUN" -eq 0 ]]; then
    case "${ANDLERD_LISTEN_ADDR:-127.0.0.1:50051}" in
        127.0.0.1:* | localhost:* | "[::1]":*) verify_eligible=1 ;;
    esac
fi

# Steps this run will show: install binaries, plus download (release), the
# optional package set, the service, and the `andler doctor` pass. A dry run
# prints only what it would do, so it counts only those.
if [[ "$DRY_RUN" -eq 1 ]]; then
    TOTAL_STEPS=$WITH_OPTIONAL
else
    TOTAL_STEPS=1
    [[ "$FROM_RELEASE" -eq 1 ]] && TOTAL_STEPS=$((TOTAL_STEPS + 1))
    [[ "$WITH_OPTIONAL" -eq 1 ]] && TOTAL_STEPS=$((TOTAL_STEPS + 1))
    [[ "$wants_service" -eq 1 ]] && TOTAL_STEPS=$((TOTAL_STEPS + 1))
    [[ "$verify_eligible" -eq 1 ]] && TOTAL_STEPS=$((TOTAL_STEPS + 1))
fi

STEP=0
step() {
    STEP=$((STEP + 1))
    ui_step "$STEP" "$TOTAL_STEPS" "$1"
}

case "$COMPONENT" in
    both) asset_prefix="andler" ;;
    daemon) asset_prefix="andlerd" ;;
    cli) asset_prefix="andler-cli" ;;
esac

ui_banner "installer" "Android Linux Emulator & Runtime"

if [[ "$SKIP_DEPS" -eq 1 ]]; then
    ui_section "Host dependencies"
    ui_warn "skipped" "--skip-deps was given; nothing was checked"
else
    check_dependencies
    if (( ${#MISSING_REQUIRED[@]} > 0 )); then
        if [[ "$CHECK_DEPS" -eq 1 ]]; then
            exit 1
        fi
        ui_error "the host is missing required dependencies — nothing was installed"
        ui_hint "install them and re-run, or pass --skip-deps to install anyway (the features they cover will fail)"
        exit 1
    fi
fi

if [[ "$CHECK_DEPS" -eq 1 ]]; then
    ui_note "--check-deps: stopping here, nothing was installed"
    exit 0
fi

ui_section "Install plan"
ui_kv "component" "$([[ "$COMPONENT" == "both" ]] && echo "andlerd + andler" || echo "$COMPONENT")"
if [[ "$FROM_RELEASE" -eq 1 ]]; then
    ui_kv "source" "release ${RELEASE_TAG:-<latest>} (github.com/$REPO)"
else
    ui_kv "source" "local binaries (PATH or target/release)"
fi
ui_kv "bin-dir" "$BIN_DIR"
ui_kv "service" "$([[ "$wants_service" -eq 1 ]] && echo "andlerd systemd user unit" || echo "not installed")"
if [[ "$WITH_OPTIONAL" -eq 1 ]]; then
    ui_kv "optional" "$(optional_packages_to_install || true)"
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
    if [[ "$WITH_OPTIONAL" -eq 1 ]]; then
        step "Optional dependencies"
        install_optional_dependencies
    fi
    ui_note "--dry-run: nothing was downloaded, installed or written"
    printf '\n'
    exit 0
fi

mkdir -p "$BIN_DIR"
if [[ ! -w "$BIN_DIR" ]]; then
    ui_error "$BIN_DIR is not writable"
    ui_hint "pass --bin-dir with a directory you own (e.g. ~/.local/bin)"
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

src_dir=""
if [[ "$FROM_RELEASE" -eq 1 ]]; then
    host_arch="$(uname -m)"
    if [[ "$host_arch" != "x86_64" ]]; then
        ui_error "published artifacts cover linux-x86_64 only (this host is $host_arch)"
        ui_hint "build from source instead: cargo build --release -p daemon -p cli"
        exit 1
    fi

    if [[ -z "$RELEASE_TAG" ]]; then
        RELEASE_TAG="$(resolve_latest_tag)"
        if [[ -z "$RELEASE_TAG" ]]; then
            ui_error "could not determine the latest release tag of $REPO"
            ui_hint "pass it explicitly: $0 --from-release v0.1.0"
            exit 1
        fi
    fi

    asset="${asset_prefix}-${RELEASE_TAG}-linux-x86_64.tar.gz"
    scratch="$(mktemp -d)"
    download_release
    src_dir="$scratch/${asset%.tar.gz}"
fi

step "Install binaries"

if [[ "$wants_daemon" -eq 1 ]]; then
    if [[ "$FROM_RELEASE" -eq 1 ]]; then
        andlerd_src="$src_dir/andlerd"
    elif ! andlerd_src="$(resolve_local_daemon)"; then
        ui_error "could not find an andlerd binary"
        ui_hint "build one (cargo build --release -p daemon), pass its path, or use --from-release"
        exit 1
    fi
    [[ -x "$andlerd_src" ]] || { ui_error "$andlerd_src is not executable"; exit 1; }
    install -m 0755 "$andlerd_src" "$BIN_DIR/andlerd"
    ui_ok "andlerd" "$BIN_DIR/andlerd  ·  $("$BIN_DIR/andlerd" --version)"
fi

if [[ "$wants_cli" -eq 1 ]]; then
    if [[ "$FROM_RELEASE" -eq 1 ]]; then
        andler_src="$src_dir/andler"
    elif ! andler_src="$(resolve_local_cli)"; then
        ui_error "could not find an andler binary"
        ui_hint "build one (cargo build --release -p cli), or use --from-release"
        exit 1
    fi
    [[ -x "$andler_src" ]] || { ui_error "$andler_src is not executable"; exit 1; }
    install -m 0755 "$andler_src" "$BIN_DIR/andler"
    ui_ok "andler" "$BIN_DIR/andler  ·  $("$BIN_DIR/andler" --version)"
fi

if [[ "$wants_daemon" -eq 1 && "$wants_cli" -eq 1 ]]; then
    cli_version="$("$BIN_DIR/andler" --version 2>/dev/null | awk '{print $NF}')"
    daemon_version="$("$BIN_DIR/andlerd" --version 2>/dev/null | awk '{print $NF}')"
    if [[ -n "$cli_version" && -n "$daemon_version" && "$cli_version" != "$daemon_version" ]]; then
        ui_warn "version mismatch" "andler $cli_version vs andlerd $daemon_version"
        ui_fix "install both from one release: $0 --component both --from-release"
    fi
fi

if [[ "$WITH_OPTIONAL" -eq 1 ]]; then
    step "Optional dependencies"
    install_optional_dependencies || true
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        ui_warn "$BIN_DIR is not on your PATH"
        case "${SHELL:-}" in
            */fish) ui_fix "fish_add_path $BIN_DIR" ;;
            */zsh) ui_fix "echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc" ;;
            *) ui_fix "echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.bashrc" ;;
        esac
        ;;
esac

# --- the service (daemon only) ----------------------------------------------

if [[ "$wants_service" -eq 1 ]]; then
    step "Install the andlerd user service"

    if ! systemd_user_ready; then
        ui_fail "systemd user instance" "not reachable — the unit cannot be installed or started"
        ui_fix "$(fix_for systemd)"
        ui_error "the service was not installed; the binaries are in place"
        ui_hint "start the daemon yourself (andlerd) or re-run once systemd answers"
        exit 1
    fi
    ui_ok "systemd user instance" "reachable"

    # systemd accepts an absolute path or a bare command name in ExecStart; a
    # relative path yields a unit that fails to load, so resolve it here.
    ANDLERD_BIN="$(readlink -f "$BIN_DIR/andlerd")"
    mkdir -p "$UNIT_DIR"
    sed "s|^ExecStart=andlerd\$|ExecStart=$ANDLERD_BIN|" \
        "$UNIT_TEMPLATE" >"$UNIT_DEST"
    if ! grep -q "^ExecStart=$ANDLERD_BIN\$" "$UNIT_DEST"; then
        ui_error "the unit template has no 'ExecStart=andlerd' line to point at the installed binary"
        ui_hint "$UNIT_TEMPLATE and this script drifted apart — install from a matching checkout"
        exit 1
    fi
    ui_ok "unit written" "$UNIT_DEST"
    ui_kv "exec" "$ANDLERD_BIN"

    systemctl --user daemon-reload
    systemctl --user enable --now andlerd
    ui_ok "service enabled and started" "andlerd"

    # The daemon answers through the CLI this run installed; a pair that cannot
    # talk is the one thing the operator should hear about here rather than at
    # the first command.
    if [[ "$wants_cli" -eq 1 ]]; then
        daemon_addr="${ANDLERD_LISTEN_ADDR:-127.0.0.1:50051}"
        daemon_url="http://$daemon_addr"
        daemon_ready=0
        for _ in $(seq 1 20); do
            if "$BIN_DIR/andler" --daemon-addr "$daemon_url" list >/dev/null 2>&1; then
                daemon_ready=1
                break
            fi
            sleep 0.5
        done
        if (( daemon_ready )); then
            ui_ok "andlerd" "answering at $daemon_url"
        else
            ui_warn "andlerd" "not answering at $daemon_url yet"
            ui_fix "systemctl --user status andlerd   &&   journalctl --user -u andlerd -n 50"
        fi
    fi
fi

# --- verify (the CLI's own report, when a daemon answers here) ---------------

if [[ "$verify_eligible" -eq 1 ]]; then
    step "Verify"

    # The service step already probed; without it (--no-service, or a daemon
    # started by hand) this is the only probe, and it is bounded.
    daemon_url="http://${ANDLERD_LISTEN_ADDR:-127.0.0.1:50051}"
    if [[ "${daemon_ready:-}" == "" ]]; then
        daemon_ready=0
        for _ in $(seq 1 10); do
            if "$BIN_DIR/andler" --daemon-addr "$daemon_url" list >/dev/null 2>&1; then
                daemon_ready=1
                break
            fi
            sleep 0.3
        done
    fi

    if (( daemon_ready )); then
        # The authoritative version of the report this script opened with:
        # doctor also covers the base-image cache, metrics and the daemon's own
        # view. Its findings do not fail the install — the required set was
        # already checked above, and what is left here is advisory.
        if "$BIN_DIR/andler" --daemon-addr "$daemon_url" doctor; then
            ui_ok "andler doctor" "host, daemon and base images check out"
        else
            ui_warn "andler doctor" "found something to look at (above)"
            ui_fix "every line it flagged carries the command that fixes it"
        fi
    else
        ui_warn "skipped" "nothing answers at $daemon_url"
        ui_fix "run it once a daemon is up: andler doctor"
    fi
fi

# --- what this run left behind ----------------------------------------------

panel_lines=()
if [[ "$wants_daemon" -eq 1 ]]; then
    panel_lines+=("andlerd   $BIN_DIR/andlerd")
fi
if [[ "$wants_cli" -eq 1 ]]; then
    panel_lines+=("andler    $BIN_DIR/andler")
fi
if [[ "$wants_service" -eq 1 ]]; then
    panel_lines+=("unit      $UNIT_DEST")
fi
ui_panel "Installed" ${panel_lines[@]+"${panel_lines[@]}"}

ui_section "Next steps"
if [[ "$wants_service" -eq 1 ]]; then
    ui_bullet "systemctl --user status andlerd      service state"
    ui_bullet "journalctl --user -u andlerd -f      daemon log"
elif [[ "$wants_daemon" -eq 1 ]]; then
    ui_bullet "andlerd                              start the daemon (no service was installed)"
fi
if [[ "$wants_cli" -eq 1 ]]; then
    ui_bullet "andler doctor                        host, the daemon and the base images"
    ui_bullet "andler create                        build your first VM"
fi
printf '\n'
