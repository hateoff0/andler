#!/usr/bin/env bash
# Shared helpers for the e2e suites. Sourced by every tests/NN_*.sh; the
# orchestrator (docker/e2e/e2e.sh) exports the E2E_* environment.
#
# Environment:
#   E2E_WORKDIR       scratch dir shared by all suites
#   E2E_STORE_PATH    sqlite store for the test daemon
#   E2E_LISTEN_ADDR   daemon listen address (127.0.0.1:50051)
#   E2E_DAEMON_PID_FILE  file holding the daemon pid (so suites can restart it)
#   E2E_LAST_OUT / E2E_LAST_ERR  capture files for expect_* helpers

set -euo pipefail

E2E_PASS=0
E2E_FAIL=0

trap 'echo "  asserts: $E2E_PASS passed, $E2E_FAIL failed"' EXIT

: "${E2E_LAST_OUT:=$E2E_WORKDIR/last.out}"
: "${E2E_LAST_ERR:=$E2E_WORKDIR/last.err}"

andler() {
    local bin="${ANDLER_BIN:-/usr/local/bin/andler}"
    "$bin" --daemon-addr "http://${E2E_LISTEN_ADDR:?E2E_LISTEN_ADDR not set}" "$@"
}

pass() {
    E2E_PASS=$((E2E_PASS + 1))
    echo "    ok: $1"
}

fail() {
    E2E_FAIL=$((E2E_FAIL + 1))
    echo "    FAIL: $1"
    exit 1
}

# expect_ok <description> [-- <command...>]
expect_ok() {
    local desc="$1" rc
    shift
    [[ "$1" == "--" ]] && shift
    if "$@" >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR"; then
        pass "$desc"
    else
        rc=$?
        echo "    command failed: $*"
        sed 's/^/      err: /' "$E2E_LAST_ERR" | head -20
        fail "$desc (exit $rc)"
    fi
}

# expect_fail <description> [-- <command...>]
expect_fail() {
    local desc="$1" rc
    shift
    [[ "$1" == "--" ]] && shift
    if "$@" >"$E2E_LAST_OUT" 2>"$E2E_LAST_ERR"; then
        fail "$desc (expected failure, command succeeded: $*)"
    else
        rc=$?
        pass "$desc (exit $rc)"
    fi
}

expect_out_grep() {
    local desc="$1" pat="$2"
    if grep -qE -- "$pat" "$E2E_LAST_OUT"; then
        pass "$desc"
    else
        sed 's/^/      out: /' "$E2E_LAST_OUT" | head -20
        fail "$desc (pattern '$pat' not found in stdout)"
    fi
}

expect_out_nogrep() {
    local desc="$1" pat="$2"
    if grep -qE -- "$pat" "$E2E_LAST_OUT"; then
        sed 's/^/      out: /' "$E2E_LAST_OUT" | head -20
        fail "$desc (pattern '$pat' unexpectedly found in stdout)"
    else
        pass "$desc"
    fi
}

expect_err_grep() {
    local desc="$1" pat="$2"
    if grep -qE -- "$pat" "$E2E_LAST_ERR"; then
        pass "$desc"
    else
        sed 's/^/      err: /' "$E2E_LAST_ERR" | head -20
        fail "$desc (pattern '$pat' not found in stderr)"
    fi
}

expect_file() {
    local desc="$1" f="$2"
    if [[ -e "$f" ]]; then pass "$desc"; else fail "$desc ($f missing)"; fi
}

expect_no_file() {
    local desc="$1" f="$2"
    if [[ -e "$f" ]]; then fail "$desc ($f still exists)"; else pass "$desc"; fi
}

start_daemon() {
    echo "  starting andlerd (store: $E2E_STORE_PATH)"
    ANDLERD_STORE_PATH="$E2E_STORE_PATH" ANDLERD_LISTEN_ADDR="$E2E_LISTEN_ADDR" \
        RUST_LOG=info /usr/local/bin/andlerd >>"$E2E_WORKDIR/daemon.log" 2>&1 &
    echo "$!" >"$E2E_DAEMON_PID_FILE"
    for _ in $(seq 1 50); do
        if andler list >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.2
    done
    echo "    FAIL: andlerd did not become ready within 10s"
    exit 1
}

stop_daemon() {
    if [[ ! -f "$E2E_DAEMON_PID_FILE" ]]; then
        return 0
    fi
    local pid
    pid="$(cat "$E2E_DAEMON_PID_FILE")"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "  stopping andlerd (pid $pid)"
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    rm -f "$E2E_DAEMON_PID_FILE"
}

# create_linux <fixtures-dir> <name> — writes a headless LinuxVM TOML with
# fresh fixture files and creates the instance; echoes the (short) instance id.
create_linux() {
    local dir="$1" name="$2"
    mkdir -p "$dir"
    qemu-img create -f qcow2 "$dir/disk.qcow2" 2G >/dev/null 2>&1
    touch "$dir/empty.iso"
    qemu-img create -f raw "$dir/VARS.fd" 4M >/dev/null 2>&1
    cat > "$dir/instance.toml" <<EOF
name = "$name"
iso_path = "$dir/empty.iso"
disk_path = "$dir/disk.qcow2"
ovmf_vars_path = "$dir/VARS.fd"

# Defaults (reference_default()) target a desktop with real GPU/sound/X server.
# DisplayEngine::None is true headless — no X11/Wayland dependency needed.
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
    andler create --file "$dir/instance.toml" | sed -n 's/.*(\(.*\))/\1/p'
}

# create_android <fixtures-dir> <name> <base-image> — AndroidVm fixture with an
# OVMF vars template and a linked overlay (so shared-base clones can work);
# echoes the (short) instance id.
create_android() {
    local dir="$1" name="$2" base="$3"
    mkdir -p "$dir"
    qemu-img create -f raw "$dir/OVMF_VARS.template.fd" 4M >/dev/null 2>&1
    andler create \
        --kind android \
        --name "$name" \
        --android-version 13 \
        --base-image-path "$base" \
        --instances-root "$dir/android-instances" \
        --ovmf-vars-template "$dir/OVMF_VARS.template.fd" \
        --linked-overlay | sed -n 's/.*(\(.*\))/\1/p'
}

# Valid-format instance id that no instance owns — exercises the daemon's
# not-found path (malformed refs are rejected by the CLI before reaching it).
UNKNOWN_ID="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
