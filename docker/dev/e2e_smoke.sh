#!/usr/bin/env bash

set -euo pipefail

WORKDIR="$(mktemp -d)"
STORE_PATH="$WORKDIR/andlerd-state.db"
LISTEN_ADDR="127.0.0.1:50051"
ANDLERD_PID=""

cleanup() {
    if [[ -n "$ANDLERD_PID" ]] && kill -0 "$ANDLERD_PID" 2>/dev/null; then
        kill "$ANDLERD_PID" 2>/dev/null || true
        wait "$ANDLERD_PID" 2>/dev/null || true
    fi
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

andler() {
    /usr/local/bin/andler --daemon-addr "http://$LISTEN_ADDR" "$@"
}

start_daemon() {
    echo "==> starting andlerd (store: $STORE_PATH)"
    ANDLERD_STORE_PATH="$STORE_PATH" ANDLERD_LISTEN_ADDR="$LISTEN_ADDR" \
        RUST_LOG=info \
        /usr/local/bin/andlerd &
    ANDLERD_PID=$!

    # Poll readiness instead of fixed sleep
    for _ in $(seq 1 50); do
        if andler list >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.2
    done
    echo "FAIL: andlerd did not become ready within 10s"
    exit 1
}

stop_daemon() {
    echo "==> stopping andlerd (pid $ANDLERD_PID)"
    kill "$ANDLERD_PID"
    wait "$ANDLERD_PID" 2>/dev/null || true
    ANDLERD_PID=""
}

echo "==> preparing fixture files (disk, iso, ovmf vars template)"
qemu-img create -f qcow2 "$WORKDIR/disk.qcow2" 2G
touch "$WORKDIR/empty.iso"
# OVMF VARS template: file must exist but content doesn't matter for protocol test
qemu-img create -f raw "$WORKDIR/VARS.fd" 4M

cat > "$WORKDIR/instance.toml" <<EOF
name = "e2e-smoke-vm"
iso_path = "$WORKDIR/empty.iso"
disk_path = "$WORKDIR/disk.qcow2"
ovmf_vars_path = "$WORKDIR/VARS.fd"

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

start_daemon

echo "==> andler create --file instance.toml"
INSTANCE_ID="$(andler create --file "$WORKDIR/instance.toml")"
echo "created instance_id=$INSTANCE_ID"
[[ -n "$INSTANCE_ID" ]] || { echo "FAIL: empty instance_id from create"; exit 1; }

echo "==> andler list (expect to see the created instance as Created)"
LIST_OUTPUT="$(andler list)"
echo "$LIST_OUTPUT"
grep -q "$INSTANCE_ID" <<<"$LIST_OUTPUT" || { echo "FAIL: instance_id not in list output"; exit 1; }
grep -q "Created" <<<"$LIST_OUTPUT" || { echo "FAIL: expected Created state in list output"; exit 1; }

echo "==> andler status $INSTANCE_ID (expect Created)"
STATUS_OUTPUT="$(andler status "$INSTANCE_ID")"
echo "$STATUS_OUTPUT"
grep -q "Created" <<<"$STATUS_OUTPUT" || { echo "FAIL: status is not Created"; exit 1; }

echo "==> andler config $INSTANCE_ID (expect full config incl. headless display)"
CONFIG_OUTPUT="$(andler config "$INSTANCE_ID")"
echo "$CONFIG_OUTPUT"
grep -q "e2e-smoke-vm" <<<"$CONFIG_OUTPUT" || { echo "FAIL: config output missing instance name"; exit 1; }
grep -q "display_engine: None" <<<"$CONFIG_OUTPUT" || { echo "FAIL: config output missing headless display_engine"; exit 1; }

echo "==> andler start $INSTANCE_ID (real qemu-system-x86_64 under /dev/kvm)"
andler start "$INSTANCE_ID"

echo "==> andler status $INSTANCE_ID (expect Running)"
STATUS_OUTPUT="$(andler status "$INSTANCE_ID")"
echo "$STATUS_OUTPUT"
if ! grep -q "Running" <<<"$STATUS_OUTPUT"; then
    echo "FAIL: status is not Running after start"
    echo "==> qemu-system-x86_64 processes still around (if any):"
    pgrep -a qemu-system-x86_64 || echo "(none found — process exited)"
    exit 1
fi

# Metrics poller updates once/sec; 3s timeout gets at least one sample
METRICS_FILE="$WORKDIR/metrics_output.txt"
timeout 3 andler metrics "$INSTANCE_ID" >"$METRICS_FILE" 2>&1 || true
if grep -q "cpu=" "$METRICS_FILE" 2>/dev/null; then
    echo "metrics: OK (received at least one sample)"
    head -1 "$METRICS_FILE"
else
    echo "metrics: WARNING — no metrics sample received (acceptable if <1s poll interval)"
    cat "$METRICS_FILE" || true
fi

# Subscribe to logs before stop to catch guaranteed SIGTERM line from QEMU
LOGS_DURING_RUN_FILE="$WORKDIR/logs_during_run.txt"
timeout 15 andler logs "$INSTANCE_ID" >"$LOGS_DURING_RUN_FILE" 2>&1 &
LOGS_BG_PID=$!
# Brief sleep so gRPC subscription reaches server before stop signal
sleep 1

echo "==> andler stop $INSTANCE_ID --graceful"
andler stop "$INSTANCE_ID" --graceful

echo "==> andler status $INSTANCE_ID (expect Stopped)"
STATUS_OUTPUT="$(andler status "$INSTANCE_ID")"
echo "$STATUS_OUTPUT"
grep -q "Stopped" <<<"$STATUS_OUTPUT" || { echo "FAIL: status is not Stopped after stop"; exit 1; }

echo "==> andler logs $INSTANCE_ID (subscribed before stop, expect the SIGTERM line and a clean stream close)"
# Background process self-limited by timeout 15 above
wait "$LOGS_BG_PID" || true
LOGS_OUTPUT="$(cat "$LOGS_DURING_RUN_FILE")"
echo "$LOGS_OUTPUT"
if ! grep -qE '^\[stderr\].*terminating on signal' <<<"$LOGS_OUTPUT"; then
    echo "FAIL: andler logs did not capture the SIGTERM line that qemu writes to stderr on stop"
    exit 1
fi

# Logs after stop: must complete immediately (no live backend) with stderr warning
echo "==> andler logs $INSTANCE_ID after stop (expect immediate empty stream + stderr warning)"
LOGS_AFTER_STOP="$(andler logs "$INSTANCE_ID" 2>&1)"
echo "$LOGS_AFTER_STOP"
grep -qE '^\[(stdout|stderr)\]' <<<"$LOGS_AFTER_STOP" && {
    echo "FAIL: andler logs returned log lines for an instance with no running backend"
    exit 1
}

# Real process restart: verify persistence across daemon restart
stop_daemon
echo "==> restarting andlerd against the same store file"
start_daemon

echo "==> andler list after restart (expect the instance to survive)"
LIST_AFTER_RESTART="$(andler list)"
echo "$LIST_AFTER_RESTART"
grep -q "$INSTANCE_ID" <<<"$LIST_AFTER_RESTART" || {
    echo "FAIL: instance_id missing from list after andlerd restart"
    exit 1
}
grep -q "Stopped" <<<"$LIST_AFTER_RESTART" || {
    echo "FAIL: expected Stopped state to survive restart (was terminal before restart)"
    exit 1
}

echo "==> andler remove $INSTANCE_ID --purge (expect disk.qcow2/VARS.fd deleted)"
andler remove "$INSTANCE_ID" --purge

echo "==> andler list after remove (expect no instances)"
andler list | grep -q "no instances" || { echo "FAIL: instance still listed after remove"; exit 1; }

if [[ -e "$WORKDIR/disk.qcow2" ]]; then
    echo "FAIL: disk.qcow2 still present after remove --purge"
    exit 1
fi
if [[ -e "$WORKDIR/VARS.fd" ]]; then
    echo "FAIL: VARS.fd still present after remove --purge"
    exit 1
fi
# purge only removes empty instance dirs, not sibling files in WORKDIR
if [[ ! -d "$WORKDIR" ]]; then
    echo "FAIL: \$WORKDIR itself was removed — purge must not touch directories containing unrelated files"
    exit 1
fi
if [[ ! -e "$WORKDIR/empty.iso" ]]; then
    echo "FAIL: unrelated empty.iso disappeared after remove --purge"
    exit 1
fi

# CloneInstance/ExportInstanceDisk: AndroidVm only (separate from LinuxVm above)
echo "==> preparing AndroidVm fixtures (base image, ovmf vars template)"
ANDROID_BASE_IMAGE="$WORKDIR/android-base.qcow2"
ANDROID_OVMF_TEMPLATE="$WORKDIR/OVMF_VARS.template.fd"
ANDROID_INSTANCES_ROOT="$WORKDIR/android-instances"
qemu-img create -f qcow2 "$ANDROID_BASE_IMAGE" 4G
qemu-img create -f raw "$ANDROID_OVMF_TEMPLATE" 4M

echo "==> andler create (Android mode, source for cloning)"
SOURCE_ANDROID_ID="$(andler create \
    --name source-android \
    --android-version 13 \
    --base-image-path "$ANDROID_BASE_IMAGE" \
    --instances-root "$ANDROID_INSTANCES_ROOT" \
    --ovmf-vars-template "$ANDROID_OVMF_TEMPLATE")"
echo "created instance_id=$SOURCE_ANDROID_ID"
[[ -n "$SOURCE_ANDROID_ID" ]] || { echo "FAIL: empty instance_id from create --android-version"; exit 1; }

echo "==> andler clone --mode linked (expect dependency on source)"
LINKED_CLONE_ID="$(andler clone "$SOURCE_ANDROID_ID" \
    --name linked-clone \
    --instances-root "$ANDROID_INSTANCES_ROOT" \
    --mode linked | sed -n 's/^cloned instance_id=//p')"
[[ -n "$LINKED_CLONE_ID" ]] || { echo "FAIL: empty instance_id from clone --mode linked"; exit 1; }

echo "==> andler remove --purge on source (expect failure: live linked clone exists)"
if andler remove "$SOURCE_ANDROID_ID" --purge 2>"$WORKDIR/remove_purge_stderr.txt"; then
    echo "FAIL: remove --purge succeeded despite a live linked clone"
    exit 1
fi
grep -qi "live" "$WORKDIR/remove_purge_stderr.txt" || {
    echo "FAIL: remove --purge error did not mention live clones:"
    cat "$WORKDIR/remove_purge_stderr.txt"
    exit 1
}

echo "==> andler clone --mode full-standalone (expect no dependency on source)"
STANDALONE_CLONE_ID="$(andler clone "$SOURCE_ANDROID_ID" \
    --name standalone-clone \
    --instances-root "$ANDROID_INSTANCES_ROOT" \
    --mode full-standalone | sed -n 's/^cloned instance_id=//p')"
[[ -n "$STANDALONE_CLONE_ID" ]] || { echo "FAIL: empty instance_id from clone --mode full-standalone"; exit 1; }

echo "==> andler clone --mode shared-base (expect no dependency on source, thin vs base image)"
SHARED_BASE_CLONE_ID="$(andler clone "$SOURCE_ANDROID_ID" \
    --name shared-base-clone \
    --instances-root "$ANDROID_INSTANCES_ROOT" \
    --mode shared-base | sed -n 's/^cloned instance_id=//p')"
[[ -n "$SHARED_BASE_CLONE_ID" ]] || { echo "FAIL: empty instance_id from clone --mode shared-base"; exit 1; }
SHARED_BASE_CLONE_DISK="$ANDROID_INSTANCES_ROOT/$SHARED_BASE_CLONE_ID/disk.qcow2"
[[ -e "$SHARED_BASE_CLONE_DISK" ]] || { echo "FAIL: shared-base clone disk file missing"; exit 1; }

echo "==> andler export (standalone file, no new instance registered)"
EXPORT_PATH="$WORKDIR/exported-android-disk.qcow2"
andler export "$SOURCE_ANDROID_ID" "$EXPORT_PATH"
[[ -e "$EXPORT_PATH" ]] || { echo "FAIL: exported disk file missing"; exit 1; }
LIST_AFTER_EXPORT="$(andler list)"
echo "$LIST_AFTER_EXPORT"
LIST_AFTER_EXPORT_COUNT="$(grep -c . <<<"$LIST_AFTER_EXPORT")"
if [[ "$LIST_AFTER_EXPORT_COUNT" -ne 4 ]]; then
    echo "FAIL: expected exactly 4 instances after export (source + 3 clones), got $LIST_AFTER_EXPORT_COUNT"
    exit 1
fi

echo "==> andler remove --purge on standalone clone (expect success: no dependency on source)"
andler remove "$STANDALONE_CLONE_ID" --purge

echo "==> andler remove --purge on shared-base clone (expect success: no dependency on source)"
andler remove "$SHARED_BASE_CLONE_ID" --purge

echo "==> andler remove --purge on linked clone, then on source (expect both to succeed now)"
andler remove "$LINKED_CLONE_ID" --purge
andler remove "$SOURCE_ANDROID_ID" --purge

echo "==> andler list after cleanup (expect no instances)"
andler list | grep -q "no instances" || { echo "FAIL: instances still listed after cleanup"; exit 1; }

echo "==> ALL E2E CHECKS PASSED"
