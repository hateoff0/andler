# andler-cli

Бинарник `andler` — тонкий gRPC-клиент к `andlerd` через `andler-rpc`. Никакой
бизнес-логики здесь — она целиком в `andler-daemon`/`andler-core`. Каждая
подкоманда формирует один gRPC-запрос и печатает ответ.

## Команды (реализовано)

- `andler create-android --name ... --android-version 13 --gapps --base-image-path ... --instances-root ... --ovmf-vars-template ...`
- `andler start <instance_id>`
- `andler pause <instance_id>`
- `andler resume <instance_id>`
- `andler stop <instance_id> [--graceful]`
- `andler status <instance_id>`

Адрес демона: `--daemon-addr`, либо переменная окружения `ANDLERD_ADDR`,
по умолчанию `http://127.0.0.1:50051` (соответствует дефолту `andlerd`,
см. `andler-daemon/src/main.rs`).

## Команды (план, не реализовано)

`list`, `snapshot`, и `create` для произвольного `InstanceConfig`
(не через `AndroidProfile`) — ждут соответствующих gRPC-методов в
`andler-rpc` (см. его README про то, почему их там пока нет).
