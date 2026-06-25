# andler-qemu

Реализация `HypervisorBackend` (из `andler-core`) поверх процесса QEMU. Источник
истины по флагам и устройствам — рабочий референсный `scripts/start.sh` в корне
репозитория; см. `docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §2.4.

## Что здесь есть

- `cmdline.rs` — **реализовано**. Чистая функция `build_args(&InstanceConfig, &Path) -> Vec<String>`
  (аргументы командной строки QEMU, включая `-qmp unix:<path>,server,nowait`), разбитая
  по блокам (`name_args`, `machine_and_cpu_args`, `memory_args`, `firmware_args`,
  `gpu_display_args`, `disk_args`, `input_args`, `network_args`, `audio_args`, `qmp_args`)
  — каждый блок тестируется отдельно сравнением с `start.sh`, без запуска реального
  процесса и без `/dev/kvm`.
- `process.rs` — **реализовано**. `QemuProcess::spawn`/`is_alive`/`terminate`/`force_kill`
  — низкоуровневое управление реальным процессом `qemu-system-x86_64` через
  `tokio::process::Command`. `terminate()` — `SIGTERM` + ожидание с таймаутом (через
  `libc::kill`, без отдельного крейта-обёртки); это временная мера до `qmp.rs` — не
  ACPI-сигнал гостю, см. документацию метода.
- `backend.rs` — **реализовано**. `QemuBackend` — `HypervisorBackend`, связывающий
  `cmdline`+`process` с единым интерфейсом. Реестр живых процессов — `HashMap` под
  `tokio::sync::Mutex`. `spawn`/`stop`/`status` реализованы полноценно;
  `pause`/`resume`/`snapshot`/`metrics_stream` возвращают `BackendError::NotImplemented`
  (требуют QMP).
- `qmp.rs` — **реализовано частично**. `QmpClient::connect` (handshake +
  `qmp_capabilities`), `pause` (`stop`), `resume` (`cont`), `query_status`
  (`query-status`) — через `serde_json` (newline-delimited JSON поверх unix-сокета).
  Snapshot (`snapshot-save` job API vs `human-monitor-command`+`savevm`) — решение
  сознательно не принято, см. ниже.
- `backend.rs` — **реализовано**. `QemuBackend` — `HypervisorBackend`, связывающий
  `cmdline`+`process`+`qmp` с единым интерфейсом. Реестр живых инстансов (процесс +
  опциональный QMP-клиент, подключаемый лениво при первом обращении) — `HashMap` под
  `tokio::sync::Mutex`. `spawn`/`stop`/`pause`/`resume`/`status` реализованы полноценно;
  `snapshot`/`metrics_stream` возвращают `BackendError::NotImplemented`.

## Чего не было в исходном `InstanceConfig` и пришлось добавить в `andler-core`

При переносе `start.sh` в `cmdline.rs` обнаружилось, что план §4.3 не выделял типы для
OVMF/UEFI firmware, audio и input/clipboard — добавлены `FirmwareConfig`, `AudioConfig`,
`InputConfig` в `andler-core::config` (см. его README и заголовочный комментарий
`config/mod.rs`). QMP-сокет (`-qmp`) не часть `InstanceConfig` — путь генерируется самим
`backend.rs` на основе `InstanceId`, передаётся в `cmdline::build_args` параметром.

## Известные ограничения текущего шага

- `status()` без живого QMP-соединения (например, сокет ещё не готов сразу после
  `spawn`, либо соединение разорвалось) деградирует до "процесс жив, точный статус
  неизвестен" — это отражено в `detail`, не маскируется молчаливым выбором `Running`.
- `qmp.rs` не различает события (asynchronous events, например `VNC_CONNECTED`) от
  ответов на команды — нет очереди событий; QMP-сообщение без `return` и без `error`
  считается ошибкой парсинга. Не проблема для текущего скоупа (`stop`/`cont`/
  `query-status` не порождают события, которые клиент мог бы получить не на свой
  запрос), но станет ограничением при добавлении подписки на события в будущем.
- Snapshot — решение между `snapshot-save` (job API, асинхронный, современный) и
  `human-monitor-command`+`savevm` (синхронный, не рекомендуется QEMU в долгосрочной
  перспективе) сознательно не принято на этом шаге.

## Что здесь НЕ реализуется

- `RenderBackend::Passthrough` (VFIO GPU passthrough) — `gpu_display_args` явно паникует,
  если этот вариант доходит до неё, но `QemuBackend::spawn` проверяет
  `RenderBackend::is_implemented()` раньше и возвращает `BackendError::InvalidConfig`, не
  допуская вызов `gpu_display_args` с этим вариантом в обычном потоке через
  `HypervisorBackend`. См. §2.3 архитектурного плана.
- `NetworkMode::Bridge`/`Isolated` — аналогично паникуют в `network_args`, так как
  `andler-net` пока не реализует их (см. его README). Это явный отказ, а не молчаливый
  фоллбек на NAT.

## Тесты

- Без `/dev/kvm` и без бинарника QEMU: `cmdline.rs` целиком, `qmp.rs` — парсинг
  структур ответа (`QmpReply`, `VmStatus`, `QueryStatusReturn`), `backend.rs` —
  валидация `Passthrough`, обработка неизвестного `BackendHandle`,
  `NotImplemented`-ветки, маппинг `VmStatus -> InstanceState`, `process.rs` — ветка
  `SpawnFailed` через гарантированно отсутствующий бинарник.
- С `/dev/kvm` и `qemu-system-x86_64` (отдельный CI job, `integration-test`
  Docker-таргет, не `unit-test`): реальный spawn/is_alive/terminate/force_kill,
  spawn→status→stop и spawn→pause→resume round-trip через `QemuBackend` (последний
  реально упражняет `qmp.rs` через живое соединение). Все помечены `#[ignore]` с
  указанием причины.

## Связанная документация

`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, разделы 2, 4.4, 6.1.1.
