#!/usr/bin/env bash
# 25 — multi-instance disk lock: two instances pointing at the same
# disk file must be refused up front with an actionable error when the
# first is already running (QEMU's own write lock would surface as an
# opaque spawn failure otherwise). Shared base images (read-only backing)
# stay legal.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/25-disk-conflict"
mkdir -p "$WORK"

# One real disk, two instance files pointing at it.
qemu-img create -f qcow2 "$WORK/shared.qcow2" 2G >/dev/null 2>&1
touch "$WORK/empty.iso"
qemu-img create -f raw "$WORK/VARS.fd" 4M >/dev/null 2>&1

write_instance_toml() { # <dir> <name>
    local dir="$1" name="$2"
    mkdir -p "$dir"
    cat > "$dir/instance.toml" <<EOF
name = "$name"
iso_path = "$WORK/empty.iso"
disk_path = "$WORK/shared.qcow2"
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
EOF
}

write_instance_toml "$WORK/a" e2e-disk-a
write_instance_toml "$WORK/b" e2e-disk-b

ID_A="$(andler create --file "$WORK/a/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
ID_B="$(andler create --file "$WORK/b/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$ID_A" && -n "$ID_B" ]] || fail "empty id from create"
pass "created two instances sharing one disk file"

expect_ok "first instance starts" -- andler start "$ID_A"
expect_ok "first status Running" -- andler status "$ID_A"
expect_out_grep "Running" "Running"

echo "  [second start refused up front]"
expect_fail "second start fails with the disk-in-use error" -- andler start "$ID_B"
expect_err_grep "error names the disk conflict" "already in use by running instance"
expect_ok "second instance did not go Error — still Created" -- andler status "$ID_B"
expect_out_grep "Created" "Created"
pass "refused second start leaves the instance Created (no raw QEMU lock failure)"

echo "  [stop frees the disk]"
expect_ok "stop the first instance" -- andler stop "$ID_A"
expect_ok "second start now succeeds" -- andler start "$ID_B"
expect_ok "second status Running" -- andler status "$ID_B"
expect_out_grep "Running" "Running"

echo "  [cleanup]"
expect_ok "stop the second" -- andler stop "$ID_B"
expect_ok "remove --purge a" -- andler remove "$ID_A" --purge
expect_ok "remove --purge b" -- andler remove "$ID_B" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
