#!/usr/bin/env bash
# 29 — memory overcommit gate: starting an instance whose guest RAM plus
# the RAM already used by running instances exceeds host physical RAM fails
# with a MemoryOvercommit error (a FailedPrecondition gRPC status) rather
# than a raw QEMU spawn failure. The gate's arithmetic is pinned by the
# daemon unit test; this suite exercises the full CLI -> gRPC -> daemon
# path end to end.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/29-overcommit"
mkdir -p "$WORK"

# Host physical RAM in bytes. MemTotal is reported in kB; the field is
# whitespace-padded, so parse the second whitespace-delimited token.
HOST_KB="$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)"
[[ -n "$HOST_KB" ]] || fail "could not read host RAM from /proc/meminfo"
HOST_BYTES=$((HOST_KB * 1024))
# A single request larger than the whole host: host RAM + 1 GiB.
OVERCOMMIT=$((HOST_BYTES + 1073741824))

pass "host RAM is ${HOST_KB} kB (${HOST_BYTES} bytes); requesting ${OVERCOMMIT} bytes"

cat > "$WORK/instance.toml" <<EOF
name = "e2e-overcommit"
disk_path = "$WORK/disk.qcow2"

[memory]
size_bytes = $OVERCOMMIT

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

qemu-img create -f qcow2 "$WORK/disk.qcow2" 2G >/dev/null 2>&1

echo "  [create registers in Created state without starting]"
ID="$(andler create --file "$WORK/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$ID" ]] || fail "empty instance id from create"
pass "created instance $ID"

echo "  [start refuses to overcommit host RAM]"
expect_fail "start refuses to overcommit host RAM" -- andler start "$ID"
expect_err_grep "overcommit error surfaced with guidance" "would exceed host memory"

echo "  [no instance is left behind]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances remain" "no instances"
