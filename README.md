# ANDLER

**ANDLER** = *Android Linux Emulator & Runtime*

Демон и CLI для управления QEMU-инстансами (Linux VM с 3D-ускорением и, в перспективе,
готовые Android-инстансы на базе Waydroid) через единый gRPC API.

## Статус

Раннняя стадия. Сейчас в работе: `andler-core` — доменная модель и абстракция backend'а
гипервизора. GPU passthrough, GUI, guest-image и пресеты — позже (см. `docs/`).

## Структура репозитория

| Путь | Что там |
|---|---|
| `crates/` | весь Rust-код: core, backend'ы (qemu/vmm), демон, CLI |
| `frontend/` | Tauri GUI-клиент (пока не начат) |
| `guest-image/` | пайплайны сборки гостевых образов с Waydroid (пока не начат) |
| `presets/` | пресеты под игры/приложения — ANDLER-Proton (пока не начат) |
| `packaging/` | сборка .deb/.rpm/AUR/AppImage (пока не начат) |
| `docs/` | архитектурная документация всего проекта |

Каждая папка внутри `crates/`, а также `frontend/`, `guest-image/`, `presets/`,
`packaging/` содержит свой `README.md` с конкретикой: что лежит здесь, что не лежит,
на какой раздел `docs/architecture/` ориентироваться.

## Документация

Общие планы и архитектурные решения — в [`docs/`](docs/). Начать стоит с
[`docs/architecture/MASTER_PLAN.md`](docs/architecture/MASTER_PLAN.md) (обзор всего
проекта) и [`docs/architecture/CORE_ARCHITECTURE_PLAN.md`](docs/architecture/CORE_ARCHITECTURE_PLAN.md)
(то, что разрабатывается прямо сейчас).

## Сборка

```bash
cargo build --workspace
cargo test --workspace
```

Требуется `/dev/kvm` (пользователь в группе `kvm`) для интеграционных тестов с реальным
QEMU; юнит-тесты `andler-core` от этого не зависят.

### В Docker (рекомендуется для воспроизводимости)

```bash
docker compose -f docker/docker-compose.yml run --rm unit-test
docker compose -f docker/docker-compose.yml run --rm integration-test  # требует /dev/kvm
```

Подробности и обоснование разделения таргетов — [`docker/README.md`](docker/README.md)
и [`docs/adr/0002-docker-build-and-test-targets.md`](docs/adr/0002-docker-build-and-test-targets.md).
