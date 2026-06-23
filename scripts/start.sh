#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

VM_NAME="linux"
RAM_SIZE="8G"
VRAM_SIZE="4096M"
DISK_SIZE="40G"

DISK_IMG="$SCRIPT_DIR/disk.qcow2"
ISO_IMG="$SCRIPT_DIR/cachyos-desktop-linux-260426.iso"

OVMF_CODE="/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd"
OVMF_VARS_TEMPLATE="/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd"
OVMF_VARS="$SCRIPT_DIR/${VM_NAME}_VARS.fd"

RESET_BOOT=false
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --reset-boot) RESET_BOOT=true; shift ;;
        *) ARGS+=("$1"); shift ;;
    esac
done

if [ ! -f "$OVMF_CODE" ]; then
    exit 1
fi

if [ "$RESET_BOOT" = true ] && [ -f "$OVMF_VARS" ]; then
    rm -f "$OVMF_VARS"
fi

if [ ! -f "$OVMF_VARS" ]; then
    cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
fi

if [ ! -f "$DISK_IMG" ]; then
    qemu-img create -f qcow2 "$DISK_IMG" "$DISK_SIZE"
fi

exec qemu-system-x86_64 \
  -name "$VM_NAME",process="$VM_NAME" \
  -machine q35,accel=kvm,usb=on \
  -cpu host,kvm=on,+topoext,migratable=no \
  -smp cpus=4,sockets=1,dies=1,cores=4,threads=1 \
  -m "$RAM_SIZE" \
  -object memory-backend-memfd,id=mem1,size="$RAM_SIZE",share=on \
  -machine memory-backend=mem1 \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$OVMF_VARS" \
  -vga none \
  -device virtio-gpu-gl,hostmem="$VRAM_SIZE",blob=true,venus=true \
  -display sdl,gl=on,show-cursor=off \
  -drive file="$DISK_IMG",format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads \
  -device virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4 \
  -drive file="$ISO_IMG",media=cdrom,if=none,id=drive-cd0 \
  -device ide-cd,drive=drive-cd0,id=cd0,bootindex=2 \
  -device virtio-tablet-pci,id=tablet0 \
  -device virtio-serial-pci \
  -device virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0 \
  -chardev qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on \
  -nic user,model=virtio-net-pci \
  -audiodev pipewire,id=snd0 \
  -device ich9-intel-hda -device hda-output,audiodev=snd0 \
  -boot menu=on "${ARGS[@]}"
