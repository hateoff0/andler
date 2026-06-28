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
- `andler remove <instance_id> [--purge]` — удаляет запись инстанса.
  Требует, чтобы инстанс был остановлен (`Created`/`Stopped`/`Error`) —
  для запущенного инстанса сначала `stop`, не делает это неявно. По
  умолчанию не удаляет файлы инстанса с диска. `--purge` дополнительно
  удаляет диск (`disk.path`) и персональную копию OVMF_VARS
  (`firmware.ovmf_vars_path`) — никогда `base_image`/`ovmf_code_path`,
  они общие для нескольких инстансов. Для `AndroidVm` это также убирает
  опустевший `instance_dir`; для `LinuxVm` с пользовательским путём
  (`andler create --file`) каталог не трогается, если в нём остались
  другие файлы.
- `andler config <instance_id>` — печатает полную конфигурацию
  инстанса (все 9 секций — CPU/память/диск/дисплей/GPU/сеть/firmware/
  audio/input — плюс `id`/`backend`/`kind`), не только сводку из
  `list`. Формат вывода читаемый, но не валидный TOML 1:1 — не
  предназначен для прямой обратной загрузки через `create --file`.
- `andler logs <instance_id>` — стримит stdout/stderr процесса
  гипервизора инстанса в реальном времени (`rpc StreamInstanceLogs`),
  каждая строка с префиксом `[stdout]`/`[stderr]`. Live-tail с момента
  подключения — не показывает строки, написанные до запуска команды
  (нет персистентной истории, см. README `andler-daemon`). Если у
  инстанса сейчас нет запущенного backend'а (ещё не стартовал, либо уже
  остановлен), команда не завершается ошибкой — печатает предупреждение
  в stderr и выходит с кодом 0, как только сервер закрывает (пустой)
  поток.
- `andler clone <source_instance_id> --name <new_name> --instances-root <path> --mode linked|full-standalone|shared-base`
  — клонирует `AndroidVm`-инстанс в новый, независимый `InstanceId`.
  Только `AndroidVm` (`LinuxVm` сейчас не поддерживается — нет
  управляемого `instances_root`, куда детерминированно положить файлы
  клона). Источник должен быть остановлен (`Created`/`Stopped`/`Error`),
  как и для `remove`. Три режима (`--mode`, см.
  `andler_core::CloneMode` за полным обоснованием каждого):
  - `linked` — дёшево и быстро (overlay с `backing_file` на диск
    источника), но клон зависит от источника: `remove --purge`
    источника откажет, пока живы его `linked`-клоны.
  - `full-standalone` — дорого по месту/времени, но результат не
    зависит ни от источника, ни от общего базового образа профиля.
  - `shared-base` — не зависит от источника физически (переживает его
    `remove --purge`), но остаётся тонким относительно общего базового
    образа — компромисс между двумя другими режимами.

  Клонирование клона разрешено (можно клонировать `linked`/`shared-base`-клон
  так же, как обычный инстанс).
- `andler export <source_instance_id> <dest_path>` — экспортирует диск
  `AndroidVm`-инстанса в самостоятельный файл по `dest_path`, для
  переноса между хостами или бэкапа. Не создаёт новый инстанс (в
  отличие от `clone --mode full-standalone`, который создаёт) — после
  экспорта `Daemon` ничего не знает про получившийся файл и не отвечает
  за него. Те же условия на источник, что у `clone`.

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

### Headless-запуск (без GPU/звука/X-сервера хоста)

Дефолты `[gpu]`/`[audio]`/`[display]` (`Venus`+`gl=on`, PipeWire, SDL)
рассчитаны на десктоп с реальным GPU и звуковым сервером. Для серверов
без дисплея или CI/Docker-сценариев (см. `docker/e2e_smoke.sh`) —
переопредели все три на минимальные зависимости:

```toml
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
```

`display_engine = "None"` — `-display none`, без обращения к
X11/Wayland хоста вообще (не то же самое, что `Spice`/`Dbus`, которые
создают поток/канал для удалённого GUI-клиента — `None` не создаёт
никакого визуального вывода).

## Команды

- `create` — создание LinuxVm-инстанса из TOML-файла
- `create-android` — создание AndroidVm-инстанса из профиля
- `start`/`stop`/`pause`/`resume` — управление жизненным циклом
- `status` — текущий статус инстанса
- `list` — список всех инстансов
- `config` — полная конфигурация инстанса
- `logs` — live-tail логов процесса
- `clone` — клонирование инстанса
- `export` — экспорт диска в файл
- `snapshot` — управление снапшотами (create/restore/delete/list)
- `remove` — удаление инстанса
