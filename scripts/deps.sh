#!/usr/bin/env bash
# Distribution facts shared by scripts/install.sh and scripts/uninstall.sh:
# which package family this host belongs to, which package ships each
# dependency, and the install/remove command lines built from them.
#
# Sourced, never executed. Callers must `set -euo pipefail` first.
#
# The package names are the ones the README's *Host Requirements & Dependencies*
# table lists; keep the two in step when a distribution renames something.

# Prints one of: arch | debian | fedora | unknown
deps_detect_family() {
    local id="" like=""
    if [[ -r /etc/os-release ]]; then
        id="$(sed -n 's/^ID="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' /etc/os-release | head -n 1)"
        like="$(sed -n 's/^ID_LIKE="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' /etc/os-release | head -n 1)"
    fi
    case " $id $like " in
        *" arch "* | *" cachyos "* | *" manjaro "* | *" endeavouros "* | *" garuda "*) printf 'arch' ;;
        *" debian "* | *" ubuntu "* | *" linuxmint "* | *" pop "* | *" kali "* | *" zorin "*) printf 'debian' ;;
        *" fedora "* | *" rhel "* | *" centos "* | *" rocky "* | *" almalinux "* | *" nobara "*) printf 'fedora' ;;
        *) printf 'unknown' ;;
    esac
}

# pkg_names <dependency key> -> "<arch>|<debian>|<fedora>", "-" where the
# dependency is not a package (a kernel feature, a device, a capability).
pkg_names() {
    case "$1" in
        qemu) printf 'qemu-system-x86|qemu-system-x86|qemu-kvm' ;;
        qemu-img) printf 'qemu-img|qemu-utils|qemu-img' ;;
        ovmf) printf 'edk2-ovmf|ovmf|edk2-ovmf' ;;
        tar-release | tar) printf 'tar|tar|tar' ;;
        sha256-release) printf 'coreutils|coreutils|coreutils' ;;
        download) printf 'curl|curl|curl' ;;
        systemd) printf 'systemd|systemd|systemd' ;;
        ip) printf 'iproute2|iproute2|iproute2' ;;
        unshare) printf 'util-linux|util-linux|util-linux' ;;
        passt) printf 'passt|passt|passt' ;;
        guestfish) printf 'libguestfs|libguestfs-tools|guestfs-tools' ;;
        debugfs) printf 'e2fsprogs|e2fsprogs|e2fsprogs' ;;
        oras) printf -- '-|-|-' ;;
        lspci) printf 'pciutils|pciutils|pciutils' ;;
        glxinfo) printf 'mesa-utils|mesa-utils|mesa-demos' ;;
        nvidia-smi) printf 'nvidia-utils|nvidia-driver|akmod-nvidia' ;;
        *) printf -- '-|-|-' ;;
    esac
}

# pkg_names_for_family <key> <family> -> that family's package name, or "-"
pkg_names_for_family() {
    local arch deb fed
    IFS='|' read -r arch deb fed <<<"$(pkg_names "$1")"
    case "$2" in
        arch) printf '%s' "$arch" ;;
        debian) printf '%s' "$deb" ;;
        fedora) printf '%s' "$fed" ;;
        *) printf '%s' "Arch: $arch / Debian: $deb / Fedora: $fed" ;;
    esac
}

# pkg_install_command <family> <package...> — empty for an unknown family.
pkg_install_command() {
    local family="$1"
    shift
    case "$family" in
        arch) printf 'sudo pacman -S --needed --noconfirm %s' "$*" ;;
        debian) printf 'sudo apt install -y %s' "$*" ;;
        fedora) printf 'sudo dnf install -y %s' "$*" ;;
        *) printf '' ;;
    esac
}

# pkg_remove_command <family> <interactive|yes> <package...> — the interactive
# form lets the package manager ask, which is what a destructive removal wants.
pkg_remove_command() {
    local family="$1" mode="$2"
    shift 2
    case "$family:$mode" in
        arch:yes) printf 'sudo pacman -R --noconfirm %s' "$*" ;;
        arch:*) printf 'sudo pacman -R %s' "$*" ;;
        debian:yes) printf 'sudo apt remove -y %s' "$*" ;;
        debian:*) printf 'sudo apt remove %s' "$*" ;;
        fedora:yes) printf 'sudo dnf remove -y %s' "$*" ;;
        fedora:*) printf 'sudo dnf remove %s' "$*" ;;
        *) printf '' ;;
    esac
}
