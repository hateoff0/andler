#!/usr/bin/env bash
# 26 — CPU pinning (cpu.affinity): the QEMU process is pinned to the
# configured host CPUs via taskset (verified through /proc
# Cpus_allowed_list), overlapping pins between running instances are
# refused up front, and an affinity index beyond the host CPU count is
# rejected before spawn.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/26-affinity"
mkdir -p "$WORK"

HOST_CPUS="$(nproc)"
[[ "$HOST_CPUS" -ge 4 ]] || fail "this suite needs at least 4 host CPUs, got $HOST_CPUS"

write_instance_toml() { # <dir> <name> <affinity-list>
    local dir="$1" name="$2" affinity="$3"
    mkdir -p "$dir"
    qemu-img create -f qcow2 "$dir/disk.qcow2" 2G >/dev/null 2>&1
    touch "$dir/empty.iso"
    qemu-img create -f raw "$dir/VARS.fd" 4M >/dev/null 2>&1
    cat > "$dir/instance.toml" <<EOF
name = "$name"
iso_path = "$dir/empty.iso"
disk_path = "$dir/disk.qcow2"
ovmf_vars_path = "$dir/VARS.fd"

[cpu]
cores = 2
sockets = 1
threads = 1
affinity = [$affinity]
priority = "Normal"

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
}

write_instance_toml "$WORK/a" e2e-aff-a "0, 1"
write_instance_toml "$WORK/b" e2e-aff-b "1, 2"
write_instance_toml "$WORK/c" e2e-aff-c "2, 3"

ID_A="$(andler create --file "$WORK/a/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
ID_B="$(andler create --file "$WORK/b/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
ID_C="$(andler create --file "$WORK/c/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$ID_A" && -n "$ID_B" && -n "$ID_C" ]] || fail "empty id from create"
pass "created three pinned instances"

echo "  [pinning is applied to the QEMU process]"
expect_ok "start the first pinned instance" -- andler start "$ID_A"
QEMU_PID="$(pgrep -f "qemu-system.*e2e-aff-a" | head -1 || true)"
[[ -n "$QEMU_PID" ]] || fail "could not find the qemu process for the pinned instance"
pass "found qemu pid $QEMU_PID"
CPUS_ALLOWED="$(awk '/Cpus_allowed_list/{print $2}' "/proc/$QEMU_PID/status")"
[[ "$CPUS_ALLOWED" == "0-1" ]] || fail "expected Cpus_allowed_list 0-1, got $CPUS_ALLOWED"
pass "qemu process pinned to CPUs 0-1 (Cpus_allowed_list=$CPUS_ALLOWED)"

echo "  [overlapping pin refused up front]"
expect_fail "overlapping start fails" -- andler start "$ID_B"
expect_err_grep "error names the pinned CPU" "is pinned by running instance"
expect_ok "overlapped instance untouched — still Created" -- andler status "$ID_B"
expect_out_grep "Created" "Created"
pass "overlapping pin refused before any spawn"

echo "  [disjoint pin starts fine]"
expect_ok "disjoint start succeeds" -- andler start "$ID_C"
QEMU_PID_C="$(pgrep -f "qemu-system.*e2e-aff-c" | head -1 || true)"
CPUS_ALLOWED_C="$(awk '/Cpus_allowed_list/{print $2}' "/proc/$QEMU_PID_C/status")"
[[ "$CPUS_ALLOWED_C" == "2-3" ]] || fail "expected Cpus_allowed_list 2-3, got $CPUS_ALLOWED_C"
pass "second pinned qemu restricted to its own CPUs (Cpus_allowed_list=$CPUS_ALLOWED_C)"

echo "  [affinity index beyond host CPU count rejected]"
write_instance_toml "$WORK/d" e2e-aff-d "$HOST_CPUS"
ID_D="$(andler create --file "$WORK/d/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
expect_fail "out-of-range affinity start fails" -- andler start "$ID_D"
expect_err_grep "error explains the range" "this host exposes"
pass "out-of-range affinity rejected before spawn"

echo "  [cleanup]"
expect_ok "stop a" -- andler stop "$ID_A"
expect_ok "stop c" -- andler stop "$ID_C"
expect_ok "remove --purge a" -- andler remove "$ID_A" --purge
expect_ok "remove --purge b" -- andler remove "$ID_B" --purge
expect_ok "remove --purge c" -- andler remove "$ID_C" --purge
expect_ok "remove --purge d" -- andler remove "$ID_D" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
