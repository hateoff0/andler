#!/usr/bin/env bash
# 24 — autostart: instances marked `autostart = true` in their
# instance.toml are started automatically when the daemon restarts, through
# the same start path as `andler start`; instances without the flag stay
# Stopped. No retry loop: a failing autostart leaves the instance in Error.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/24-autostart"
mkdir -p "$WORK"

# --- create an instance with autostart = true in its --file TOML ---------
mkdir -p "$WORK/auto"
qemu-img create -f qcow2 "$WORK/auto/disk.qcow2" 2G >/dev/null 2>&1
touch "$WORK/auto/empty.iso"
qemu-img create -f raw "$WORK/auto/VARS.fd" 4M >/dev/null 2>&1
cat > "$WORK/auto/instance.toml" <<EOF
name = "e2e-autostart"
iso_path = "$WORK/auto/empty.iso"
disk_path = "$WORK/auto/disk.qcow2"
ovmf_vars_path = "$WORK/auto/VARS.fd"
autostart = true

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
AUTO_ID="$(andler create --file "$WORK/auto/instance.toml" | sed -n 's/.*(\(.*\))/\1/p')"
[[ -n "$AUTO_ID" ]] || fail "empty id from autostart create"
pass "created instance with autostart=true ($AUTO_ID)"

# --- create a second instance without the flag ---------------------------
NO_ID="$(create_linux "$WORK/noauto" e2e-no-autostart)"
[[ -n "$NO_ID" ]] || fail "empty id from non-autostart create"
pass "created instance without autostart ($NO_ID)"

expect_ok "both start cleanly first time" -- andler start "$AUTO_ID"
expect_ok "non-autostart start" -- andler start "$NO_ID"
expect_ok "stop both before the restart" -- andler stop "$AUTO_ID"
expect_ok "stop non-autostart" -- andler stop "$NO_ID"

echo "  [daemon restart]"
stop_daemon
start_daemon

# --- autostart=true must come up by itself -------------------------------
RUNNING=1
for _ in $(seq 1 60); do
    if andler status "$AUTO_ID" --json >"$WORK/auto-status.json" 2>/dev/null &&
        grep -q '"Running"' "$WORK/auto-status.json"; then
        RUNNING=0
        break
    fi
    sleep 0.5
done
[[ "$RUNNING" -eq 0 ]] || {
    echo "    --- status ---"
    andler status "$AUTO_ID" 2>&1 | sed 's/^/      /' || true
    echo "    --- daemon.log tail ---"
    tail -20 "$E2E_WORKDIR/daemon.log" | sed 's/^/      /'
    fail "autostart instance did not reach Running within 30s of daemon restart"
}
pass "autostart=true instance reached Running after daemon restart"

# --- exactly one QEMU process for it (no double start) -------------------
QEMU_COUNT="$(pgrep -fc "qemu-system.*e2e-autostart" || true)"
[[ "$QEMU_COUNT" -eq 1 ]] || fail "expected exactly one qemu for the autostart instance, got $QEMU_COUNT"
pass "exactly one qemu process for the autostarted instance"

# --- autostart=false stays Stopped ---------------------------------------
expect_ok "non-autostart status after restart" -- andler status "$NO_ID"
expect_out_grep "non-autostart stays Stopped" "Stopped"
pass "autostart=false instance stayed Stopped after daemon restart"

# --- config set autostart toggles the flag (keypath) ---------------------
expect_ok "config set autostart false" -- andler config set "$AUTO_ID" autostart false
expect_ok "config view shows autostart false" -- andler config view "$AUTO_ID"
expect_out_grep "autostart=false shown in config view" "autostart: false"

expect_ok "stop the autostarted instance" -- andler stop "$AUTO_ID"
expect_ok "config set autostart true again" -- andler config set "$AUTO_ID" autostart true

echo "  [second daemon restart: still autostarts]"
stop_daemon
start_daemon
RUNNING=1
for _ in $(seq 1 60); do
    if andler status "$AUTO_ID" --json >"$WORK/auto-status2.json" 2>/dev/null &&
        grep -q '"Running"' "$WORK/auto-status2.json"; then
        RUNNING=0
        break
    fi
    sleep 0.5
done
[[ "$RUNNING" -eq 0 ]] || fail "autostart did not fire on the second restart"
pass "autostart fires again after config-set toggle + second restart"

echo "  [cleanup]"
expect_ok "stop autostart instance" -- andler stop "$AUTO_ID"
expect_ok "remove --purge autostart" -- andler remove "$AUTO_ID" --purge
expect_ok "remove --purge non-autostart" -- andler remove "$NO_ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
