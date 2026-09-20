#!/usr/bin/env bash
# 17 — port forwards: network.port_forwards plumbing through TOML → config
# → QEMU hostfwd, and andler connect --level ssh/adb client dispatch.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/17-portfwd"
mkdir -p "$WORK"

qemu-img create -f qcow2 "$WORK/disk.qcow2" 2G >/dev/null 2>&1
touch "$WORK/empty.iso"
qemu-img create -f raw "$WORK/VARS.fd" 4M >/dev/null 2>&1
cat > "$WORK/instance.toml" <<EOF
name = "e2e-portfwd"
iso_path = "$WORK/empty.iso"
disk_path = "$WORK/disk.qcow2"
ovmf_vars_path = "$WORK/VARS.fd"

[gpu]
render_backend = "cpu"
hostmem_bytes = 67108864
blob = false
gl = false

[display]
resolution = { width = 1024, height = 768 }
dpi = 96
fps_limit = 0
engine = "none"
fullscreen = false

[audio]
backend = "none"

[network]
mode = "nat"
device_model = "virtio-net-pci"
[[network.port_forwards]]
protocol = "tcp"
host_port = 2222
guest_port = 22
[[network.port_forwards]]
protocol = "tcp"
host_port = 5555
guest_port = 5555
EOF

expect_ok "create --file" -- andler create --file "$WORK/instance.toml"
ID="$(andler list --json | sed -n 's/.*"id": *"\([a-f0-9]\{8\}\).*/\1/p' | head -1)"
[[ -n "$ID" ]] || fail "empty instance id"

echo "  [port forwards appear in dry-run cmdline]"
expect_ok "dry-run cmdline" -- andler create --file "$WORK/instance.toml" --dry-run
expect_out_grep "hostfwd visible" "hostfwd=tcp::2222-:22"

echo "  [start applies hostfwd]"
expect_ok "start" -- andler start "$ID"
sleep 4
# QEMU slirp binds hostfwd on the host side regardless of the guest state:
# a TCP connect to the forwarded port must succeed while the VM runs.
if timeout 3 bash -c "exec 3<>/dev/tcp/127.0.0.1/2222" 2>/dev/null; then
    pass "hostfwd port accepts connections"
else
    fail "hostfwd port 2222 is not listening while the VM runs"
fi

echo "  [connect --level ssh without ssh binary degrades cleanly]"
if command -v ssh >/dev/null 2>&1; then
    # ssh exists in container; run it with a short timeout — it should try to
    # connect to the forwarded port. No guest sshd → exit 255 is fine.
    set +e
    timeout 3 andler connect "$ID" --level ssh 2>"$E2E_LAST_OUT" < /dev/null
    rc=$?
    set -e
    [[ "$rc" -eq 255 || "$rc" -eq 124 || "$rc" -eq 0 ]] || \
        fail "connect --level ssh exited with unexpected rc=$rc (stderr: $(cat "$E2E_LAST_OUT"))"
else
    expect_fail "connect --level ssh without ssh" -- andler connect "$ID" --level ssh
    expect_err_grep "ssh missing" "is it installed"
fi

echo "  [host port conflicts across instances]"
# A second instance forwarding the same host port must be refused at start
# with an actionable message, not left to QEMU's opaque bind error.
WORK2="$WORK/second"
mkdir -p "$WORK2"
for f in disk.qcow2 empty.iso VARS.fd; do
    cp "$WORK/$f" "$WORK2/$f"
done
cp "$WORK/instance.toml" "$WORK2/instance.toml"
sed -i "s/name = \"e2e-portfwd\"/name = \"e2e-portfwd-2\"/; s|$WORK/|$WORK2/|g" "$WORK2/instance.toml"
expect_ok "create second instance" -- andler create --file "$WORK2/instance.toml"
ID2="$(andler list | awk '$3 == "e2e-portfwd-2" { print $1 }' | head -1)"
[[ -n "$ID2" ]] || fail "empty second id"
expect_fail "start with a conflicting host port" -- andler start "$ID2"
expect_err_grep "conflict message names the port" "port 2222 is already forwarded"
expect_err_grep "conflict message names the holder" "running instance"

echo "  [cleanup]"
expect_ok "stop" -- andler stop "$ID" --graceful
expect_ok "remove --purge second" -- andler remove "$ID2" --purge
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
