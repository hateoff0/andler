#!/usr/bin/env bash
# 23 — online package install on a genuinely booted guest: the official
# Debian genericcloud image (baked into the e2e image at build time) plus a
# NoCloud seed disk. cloud-init brings up DHCP, installs qemu-guest-agent
# and enables it; then the default (no --offline) install/remove path runs
# against the running VM via QGA. No guestmount/userns/chroot anywhere in
# fixture preparation — the guest does its own package management.
set -u
E2E_ROOT="${E2E_ROOT:-/usr/local/share/andler-e2e}"
# shellcheck source=common.sh
source "$E2E_ROOT/tests/common.sh"

WORK="$E2E_WORKDIR/23-online-install"
mkdir -p "$WORK"

echo "  [fixture: seed + cloud instance]"
SEED="$WORK/seed"
mkdir -p "$SEED"
cat > "$SEED/user-data" <<'YAML'
#cloud-config
bootcmd:
  # The guest's resolved/DHCP DNS chain fails to resolve deb.debian.org
  # (IPv6-first answers with no usable v6 path through slirp). Pin the
  # slirp DNS server, hardcode the mirror IPv4s in /etc/hosts and force
  # apt onto IPv4 — the same workarounds the offline chroot path uses.
  - rm -f /etc/resolv.conf
  - echo nameserver 10.0.2.3 > /etc/resolv.conf
  - echo 151.101.2.132 deb.debian.org >> /etc/hosts
  - echo 151.101.130.132 security.debian.org >> /etc/hosts
  - echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99forceipv4
packages: [qemu-guest-agent]
# The deb's postinst does not enable the unit under this policy (the
# install reports "disabled or a static unit, not starting it"), so start
# it explicitly once apt is done.
runcmd:
  - systemctl enable --now qemu-guest-agent.service
YAML
# Without network config cloud-init cannot bring up the NIC (the image
# ships no default .network files), so apt inside the guest cannot
# resolve anything. DHCP via slirp is enough.
cat > "$SEED/network-config" <<'YAML'
network:
  version: 2
  ethernets:
    all-eth:
      match:
        name: en*
      dhcp4: true
YAML
: > "$SEED/meta-data"
genisoimage -quiet -output "$WORK/seed.iso" -volid cidata -joliet -rock \
    "$SEED/user-data" "$SEED/meta-data" "$SEED/network-config" \
    || fail "seed iso creation failed"

mkdir -p "$WORK/inst"
cp /opt/e2e/online-disk.qcow2 "$WORK/inst/disk.qcow2"
qemu-img create -f raw "$WORK/inst/VARS.fd" 4M >/dev/null 2>&1
# The cloud kernel has no AHCI, so the seed ISO must ride virtio-scsi.
cat > "$WORK/inst/instance.toml" <<EOF
name = "e2e-online"
iso_path = "$WORK/seed.iso"
disk_path = "$WORK/inst/disk.qcow2"
ovmf_vars_path = "$WORK/inst/VARS.fd"
cdrom_bus = "virtio"

[gpu]
render_backend = "Cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
display_engine = "None"
fullscreen = false

[audio]
backend = "None"
EOF
OID="$(andler create --file "$WORK/inst/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$OID" ]] || fail "empty id from online instance create"
pass "cloud instance created from the baked genericcloud image"

echo "  [guest boots and cloud-init installs QGA]"
expect_ok "boot the guest" -- timeout 60 andler start "$OID"
QGA_READY=0
for _ in $(seq 1 120); do
    if timeout 20 andler exec "$OID" -- /bin/true >/dev/null 2>&1; then
        QGA_READY=1
        break
    fi
    sleep 2
done
if [[ "$QGA_READY" -eq 1 ]]; then
    pass "guest agent answered after boot (cloud-init installed and enabled it)"
else
    fail "guest agent did not answer within 240s (see $E2E_WORKDIR/daemon.log)"
fi

echo "  [online package path on the running VM]"
expect_ok "online install (default path, running VM)" -- timeout 300 andler guest install hello "$OID"
expect_out_grep "online install reports success" "installed successfully"
expect_ok "exec confirms the installed binary" -- andler exec "$OID" -- /bin/sh -c 'test -x /usr/bin/hello && echo hello-present'
expect_out_grep "hello binary present" "hello-present"
expect_ok "online remove (default path, running VM)" -- timeout 300 andler guest remove hello "$OID"
expect_out_grep "online remove reports success" "removed successfully"
echo "  [online idempotency token on the running VM]"
# The `--idempotency-token` flag round-trips through the CLI -> proto -> service
# -> daemon and is used as the in-flight-operation join key on the online path.
# A repeat with the same token is idempotent; the join/refuse negative path is
# pinned by the daemon unit tests (guest_maintenance).
expect_ok "online install with a custom idempotency token" -- timeout 300 andler guest install hello "$OID" --idempotency-token tok-install-abc
expect_out_grep "install with token reports success" "installed successfully"
expect_ok "exec confirms the tokened install" -- andler exec "$OID" -- /bin/sh -c 'test -x /usr/bin/hello && echo hello-present'
expect_out_grep "hello binary present after tokened install" "hello-present"
expect_ok "online install again with the same token is idempotent" -- timeout 300 andler guest install hello "$OID" --idempotency-token tok-install-abc
expect_out_grep "repeat token install reports success" "installed successfully"
expect_ok "online remove with a custom idempotency token" -- timeout 300 andler guest remove hello "$OID" --idempotency-token tok-remove-abc
expect_out_grep "remove with token reports success" "removed successfully"

expect_ok "stop the guest" -- andler stop "$OID"
expect_ok "remove the online instance" -- andler remove "$OID" --purge

echo "  [cleanup]"
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
