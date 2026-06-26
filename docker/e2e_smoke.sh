#!/usr/bin/env bash
# Сквозная (end-to-end) проверка andlerd + andler: то, что не может
# показать ни один юнит-тест — реальный процесс andlerd, реальный TCP
# между ним и CLI-клиентом, реальный sqlite-файл на диске, реальный
# qemu-system-x86_64 под /dev/kvm, и реальный перезапуск процесса демона
# (не Daemon::restore() в памяти теста, а буквально kill + новый процесс).
#
# Запуск: docker compose -f docker/docker-compose.yml run --rm e2e
# (через target `e2e` в Dockerfile.dev — собирает workspace, ставит
# qemu-system-x86/qemu-utils, и гонит этот скрипт с --device=/dev/kvm).
#
# Падает (set -e) на первом несовпадении ожидания — где именно упало,
# видно по последней напечатанной секции "==> ...".

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

    # Поллинг готовности вместо фиксированного sleep — andlerd печатает
    # "andlerd: listening on ..." в stdout сразу после успешного bind,
    # но захватывать stdout фонового процесса в bash менее надёжно, чем
    # просто попробовать первый gRPC-вызов несколько раз подряд.
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
# Шаблон OVMF_VARS не нужен для generic CreateInstance (только для
# create-android) — firmware.ovmf_vars_path в TOML просто должен
# существовать как путь, который start_instance передаст в qemu
# аргументом -drive if=pflash; реальное содержимое не валидируется на
# этом уровне, но пустой файл небезопасен для настоящей UEFI-загрузки.
# Для целей этого smoke-теста (проверка протокола/жизненного цикла, не
# настоящей загрузки гостя) достаточно файла нужного размера.
qemu-img create -f raw "$WORKDIR/VARS.fd" 4M

cat > "$WORKDIR/instance.toml" <<EOF
name = "e2e-smoke-vm"
iso_path = "$WORKDIR/empty.iso"
disk_path = "$WORKDIR/disk.qcow2"
ovmf_vars_path = "$WORKDIR/VARS.fd"

# Дефолты GpuConfig/AudioConfig/DisplayConfig (reference_default(), см.
# README этого крейта) — Venus+gl=on, PipeWire, и SDL — рассчитаны на
# десктоп с реальным GPU/звуковым сервером/X-сервером хоста, которых в
# этом контейнере нет. DisplayEngine::None ("-display none") — настоящий
# headless-режим без какого-либо X11/Wayland-сервера хоста (раньше здесь
# был нужен виртуальный Xvfb только чтобы у SDL было куда присоединиться
# — с DisplayEngine::None эта зависимость не нужна вообще).
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

echo "==> andler stop $INSTANCE_ID --graceful"
andler stop "$INSTANCE_ID" --graceful

echo "==> andler status $INSTANCE_ID (expect Stopped)"
STATUS_OUTPUT="$(andler status "$INSTANCE_ID")"
echo "$STATUS_OUTPUT"
grep -q "Stopped" <<<"$STATUS_OUTPUT" || { echo "FAIL: status is not Stopped after stop"; exit 1; }

# --- Реальный перезапуск процесса andlerd: проверка персистентности ---
# не Daemon::restore() внутри одного процесса теста, а буквально новый
# процесс, читающий тот же sqlite-файл с диска.
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

echo "==> andler remove $INSTANCE_ID"
andler remove "$INSTANCE_ID"

echo "==> andler list after remove (expect no instances)"
andler list | grep -q "no instances" || { echo "FAIL: instance still listed after remove"; exit 1; }

echo "==> ALL E2E CHECKS PASSED"
