# docker

Сборка и тестирование ANDLER в контейнерах — для воспроизводимости (одна
версия Rust/системных либ у всех разработчиков и в CI) и для изоляции
тестов, которым нужен `/dev/kvm`, от тех, которым он не нужен.

## Файлы

- `Dockerfile.dev` — multi-stage: `builder` → `unit-test` / `integration-test` / `daemon` / `e2e`. Подробности и обоснование разделения — комментарии в самом файле.
- `docker-compose.yml` — шорткаты для локального запуска.
- `e2e_smoke.sh` — скрипт E2E-теста: поднимает andlerd, создаёт инстанс, проверяет статус, логи, метрики.

## Быстрый старт

```bash
# Сборка (первый раз или после изменения зависимостей — ~2-5 мин)
docker compose build --no-cache unit-test

# Юнит-тесты всего workspace, без /dev/kvm — работает везде, в том числе
# на CI-раннерах без виртуализации.
docker compose run --rm unit-test

# Интеграционные тесты (#[ignore]-тесты: qemu-img, QMP round-trip).
# Требует /dev/kvm на хосте.
docker compose build --no-cache integration-test
docker compose run --rm integration-test

# E2E smoke-тест: реальный andlerd + andler, TCP, sqlite.
# Требует /dev/kvm на хосте.
docker compose build --no-cache e2e
docker compose run --rm e2e
```

### Без `--no-cache`

Если зависимости не менялись, сборка из кеша занимает ~10 сек.
`--no-cache` нужен при первом запуске или после изменения `Cargo.toml`/`Cargo.lock`.

## Что запускается в каждом таргете

| Таргет | Что делает | Нужен `/dev/kvm` | Нужен `qemu-img` |
|--------|-----------|:-----------------:|:-----------------:|
| `unit-test` | `cargo test --workspace` — все тесты без `#[ignore]` | Нет | Нет |
| `integration-test` | `cargo test --workspace -- --ignored` — #[ignore]-тесты (qemu-img, QMP, spawn) | Да | Да |
| `e2e` | `e2e_smoke.sh` — реальный andlerd + andler, TCP, sqlite, restart | Да | Да |
| `daemon` | Минимальный runtime-образ andlerd (не тестовый) | Да | Да |

## Почему не один таргет на всё

`andler-core` — чистый домен, тестируется без всякого окружения. Часть
тестов `andler-qemu`/`andler-daemon` требует реального процесса QEMU и
`/dev/kvm`. Смешивание этого в один `cargo test --workspace` без разделения
таргетов означало бы, что CI на каждый PR либо тащит KVM-зависимость туда,
где она не нужна, либо игнорирует интеграционные тесты молча. Разделение
соответствует §7 архитектурного плана: быстрый `cargo test --workspace` на
каждый PR, отдельный job с `/dev/kvm` на merge в основную ветку.

## Сборка всех таргетов

```bash
# Собрать все образы сразу (unit-test + integration-test + daemon + e2e)
docker compose build --no-cache
```

## Конвенция для тестов, требующих внешних бинарников

Тест, которому нужен реальный `/dev/kvm` (andler-qemu) или просто бинарник
`qemu-img` без KVM (andler-disk), помечается `#[ignore]` с комментарием,
объясняющим причину (а не просто `#[ignore]` без пояснения — иначе
непонятно, временно тест выключен или принципиально требует окружения).
Таргет `integration-test` запускает такие тесты явно через
`cargo test -- --ignored` (там есть и `qemu-utils`, и `/dev/kvm`).
Таргет `e2e` идёт дальше — поднимает реальный andlerd и гоняет
`e2e_smoke.sh`.

## Продуктовый деплой — это НЕ Docker

`daemon`-таргет существует для разработки/CI, а не как способ доставки
`andlerd` пользователям. Конечная дистрибуция — `.deb`/`.rpm`/AUR/AppImage
(см. `packaging/`), потому что GUI-клиенту и демону на машине пользователя
нужен прямой доступ к `/dev/kvm` и дисплею хоста, что плохо сочетается с
обычным контейнерным изолированием.

## Решение проблем

### KVM: permission denied

```bash
# Проверить доступность /dev/kvm
ls -la /dev/kvm

# Добавить пользователя в группу kvm
sudo usermod -aG kvm $USER
# Перелогиниться для применения
```

### Сборка падает с ошибкой protobuf

```bash
# Убедиться, что protoc установлен (нужен для andler-rpc)
which protoc || sudo apt install protobuf-compiler
```

### Очистка Docker-кеша

```bash
docker system prune -f
docker compose build --no-cache unit-test
```
