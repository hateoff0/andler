# andler-cli

Бинарник `andler` — тонкий gRPC-клиент к `andlerd` через `andler-rpc`. Никакой
бизнес-логики здесь — она целиком в `andler-daemon`/`andler-core`. Каждая
подкоманда формирует один gRPC-запрос и печатает ответ.

## Команды (реализовано)

- `andler create --file instance.toml` — создаёт `LinuxVm`-инстанс из
  TOML-файла (см. "Формат файла `create`" ниже). Соответствует
  `rpc CreateInstance`.
- `andler create-android --name ... --android-version 13 --gapps --base-image-path ... --instances-root ... --ovmf-vars-template ...`
- `andler start <instance_id>`
- `andler pause <instance_id>`
- `andler resume <instance_id>`
- `andler stop <instance_id> [--graceful]`
- `andler status <instance_id>`
- `andler list` — печатает `instance_id`, состояние и имя каждого
  зарегистрированного инстанса (одна строка на инстанс,
  `instance_id  STATE  name`). Состояние — запись демона
  (`Daemon::list_instances`), не live backend-статус; для точного
  статуса конкретного инстанса используй `status`.
- `andler remove <instance_id>` — удаляет запись инстанса. Требует,
  чтобы инстанс был остановлен (`Created`/`Stopped`/`Error`) — для
  запущенного инстанса сначала `stop`, не делает это неявно. Не
  удаляет файлы инстанса с диска.

Адрес демона: `--daemon-addr`, либо переменная окружения `ANDLERD_ADDR`,
по умолчанию `http://127.0.0.1:50051` (соответствует дефолту `andlerd`,
см. `andler-daemon/src/main.rs`).

## Формат файла `create`

Минимальный файл — только обязательные поля, всё остальное берётся из
`andler_core::config::*::reference_default()`:

```toml
name = "my-linux-vm"
iso_path = "/home/user/isos/cachyos.iso"
disk_path = "/home/user/.local/share/andler/my-linux-vm/disk.qcow2"
ovmf_vars_path = "/home/user/.local/share/andler/my-linux-vm/VARS.fd"
```

`disk_size_gib` (целое число, не байты) переопределяет размер диска без
необходимости заполнять всю секцию `[disk]`:

```toml
disk_size_gib = 100
```

Любая секция `cpu`/`memory`/`display`/`gpu`/`network`/`audio`/`input`
переопределяется целиком — поля совпадают один-к-одному с одноимённым
типом из `andler-core::config` (см. `andler-cli/src/instance_file.rs` за
точным списком полей каждой секции и `andler-core/src/config/*.rs` за их
документацией):

```toml
[cpu]
cores = 8
sockets = 1
threads = 2
affinity = []
priority = "High"

[gpu]
hostmem_bytes = 4294967296
blob = true
gl = true

[gpu.render_backend.Venus]
```

Секцию можно опустить целиком — тогда применяется
`reference_default()` этой секции; то, что задано, заменяет дефолт
целиком, не точечно по полю (например, указав `[cpu]` без `priority`,
получишь ошибку парсинга TOML, а не `CpuPriority::Normal` по умолчанию —
секция либо полностью описана, либо отсутствует).

## Команды (план, не реализовано)

`snapshot` — ждёт соответствующего gRPC-метода в `andler-rpc`
(см. его README про то, почему его там пока нет).
