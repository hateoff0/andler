# andler-daemon

Бинарник `andlerd`. Ядро — `daemon.rs`: структура `Daemon`, которая держит
реестр backend'ов (`HashMap<BackendKind, Arc<dyn HypervisorBackend>>`) и
in-memory состояние инстансов, без сети и без персистентности.

## Что здесь есть

- `daemon.rs` — **реализовано**. `Daemon::create_instance`/`start_instance`/
  `stop_instance`/`pause_instance`/`resume_instance`/`status` — методы,
  которые соответствуют будущим gRPC-методам один-к-одному по смыслу, но
  пока вызываются прямо как обычные async-методы Rust. Каждый метод
  применяет переходы `andler_core::fsm` и делегирует нужному backend'у
  через `HypervisorBackend`. `Daemon::new()` регистрирует только
  `BackendKind::Qemu -> QemuBackend` — `Vmm` не регистрируется, пока
  `andler-vmm` остаётся пустым каркасом (см. его README).
- `Daemon::create_android_instance` — **реализовано**. Связывает
  `AndroidProfile::resolve()` (`andler-core`, чистая функция) с реальным
  созданием на диске: каталог инстанса, персональная копия `OVMF_VARS`
  (копирование системного шаблона) и overlay-диск через
  `andler-disk::overlay::create_overlay`. Получившийся `InstanceConfig`
  проходит через тот же `create_instance`, что и при прямом вызове — то
  есть та же валидация backend'а и тот же реестр. Это первый сквозной
  путь от Android-профиля до зарегистрированного инстанса.

## Что здесь НЕ реализовано (следующие слои поверх `Daemon`)

- **Сеть/gRPC** — теперь **реализовано**: `service.rs` (`DaemonService`) —
  тонкая обёртка `andler_rpc::proto::andler_service_server::AndlerService`
  поверх методов `Daemon`, по одному методу трейта на один метод `Daemon`,
  без дополнительной логики. `main.rs` поднимает её через
  `tonic::transport::Server` на `ANDLERD_LISTEN_ADDR`
  (по умолчанию `127.0.0.1:50051`, loopback осознанно — нет
  аутентификации/TLS, см. комментарий в `main.rs`). Набор методов — намеренное
  подмножество §5.1 архитектурного плана; см. README `andler-rpc` почему.
  Отображение `DaemonError -> tonic::Status` — в `service.rs`
  (`impl From<DaemonError> for Status`).
  - **`CreateInstance` (generic, `LinuxVm`)** — теперь **реализовано**:
    принимает полный `InstanceConfig` через `CreateInstanceRequest`
    (все 9 под-конфигураций), без промежуточного резолва — конвертация
    `andler_rpc::convert::TryFrom<CreateInstanceRequest> for InstanceConfig`
    уже даёт готовый конфиг, который идёт прямо в `Daemon::create_instance`.
    До этого единственным сквозным путём создания инстанса по сети был
    `CreateAndroidInstance` — теперь `LinuxVm` тоже доступен клиенту, не
    только напрямую через Rust-вызов `Daemon::create_instance` в тестах.
- **Персистентность** — теперь **реализовано**: `Daemon` опционально несёт
  `andler_store::Store` (`Daemon::new()` — без персистентности, как
  раньше; `Daemon::with_store(store)` — с персистентностью без
  восстановления; `Daemon::restore(store)` — восстанавливает все ранее
  сохранённые инстансы при старте, используется в `main.rs`).
  `create_instance` сохраняет новую запись, `start_instance`/
  `stop_instance` сохраняют каждый промежуточный и финальный переход FSM
  (включая `Starting`/`Stopping` — не только конечные `Running`/`Stopped`/
  `Error`). Ошибка записи в `store` логируется (`tracing::error!`) и не
  проваливает саму операцию — in-memory состояние остаётся источником
  истины для текущей сессии демона; см. подробное обоснование в
  комментарии `Daemon::persist_state`/`persist_new_instance`.
  `pause_instance`/`resume_instance` не персистируются отдельно — как и
  раньше, они не меняют `InstanceState` записи демона напрямую (см.
  раздел "Важные решения" ниже), поэтому нет нового состояния, которое
  нужно было бы сохранить.
  - **Восстановление и потерянные backend-хэндлы** — `Daemon::restore`
    принудительно переводит любую запись, восстановленную в
    нетерминальном состоянии (`Starting`/`Running`/`Paused`/`Stopping`),
    в `InstanceState::Error` — backend-хэндл (дескриптор живого QEMU-
    процесса) никогда не персистируется и не может быть восстановлен
    после перезапуска `andlerd`, так что оставлять такую запись как
    `Running` было бы ложью о текущем состоянии. См. подробности в
    docstring `Daemon::restore`.
  - Путь к sqlite-файлу — `ANDLERD_STORE_PATH` (по умолчанию
    `andlerd-state.db` в рабочем каталоге процесса).
- **Получение `base_image_path` для `create_android_instance`** — проверка
  кэша базового образа и скачивание (см. §4.4.2 архитектурного плана и
  `GUEST_IMAGE_PLAN.md`) остаются вне `Daemon`: метод принимает уже готовый
  путь к базовому образу, не качает его сам. Через gRPC это значит, что
  `base_image_path`/`ovmf_vars_template` в `CreateAndroidInstanceRequest` —
  пути на файловой системе хоста `andlerd`, не клиента (см. `docs/api/grpc.md`).
- **Очистка частично созданного `instance_dir` при ошибке внутри
  `create_android_instance`** (например, overlay не создался после того,
  как `VARS.fd` уже скопирован) — осознанно не реализована; TODO вместе с
  удалением инстанса/Factory Reset через `Daemon`.

## Важные решения, зафиксированные в `daemon.rs`

- Ошибка `HypervisorBackend::spawn`/`stop` переводит запись инстанса в
  `InstanceState::Error { message }`, а не оставляет её в промежуточном
  `Starting`/`Stopping` — иначе запись «застревала» бы в состоянии, из
  которого FSM не разрешает дальнейших переходов, кроме `Fail`.
- `pause_instance`/`resume_instance` не меняют `InstanceState` записи
  демона напрямую при успехе — реальное состояние гостя (`Running` vs
  `Paused`) синхронизируется через `status()`, который спрашивает backend
  (а тот — QMP `query-status`), а не предполагается на основании одной
  лишь успешной отправки команды.

## Тесты

- `daemon::tests` — `Daemon` напрямую, без сети (включая
  `create_android_instance`), плюс новый блок персистентности:
  `with_store_persists_created_instance`,
  `with_store_persists_failed_start_as_error_state`,
  `daemon_without_store_does_not_panic_on_state_transitions` (режим без
  `Store` остаётся валидным no-op путём), `restore_*` — восстановление из
  `Store` (терминальные состояния переживают restore как есть,
  нетерминальные принудительно становятся `Error`, пустой `Store` даёт
  `Daemon` без инстансов, восстановленный `Daemon` продолжает
  персистировать новые операции).
- `grpc_roundtrip_test` (`#[cfg(test)]`, не помечен `#[ignore]`) — реальный
  `tonic::transport::Server` на эфемерном `127.0.0.1`-порту + реальный
  `AndlerServiceClient` через настоящий TCP. Проверяет то, что unit-тесты
  `andler-rpc::convert` не могут: что весь путь "клиент сериализует -> сервер
  роутит на метод трейда -> зовёт `Daemon` -> сериализует ответ обратно"
  реально работает по сети, а не только компилируется. Не требует
  `qemu-img`/`/dev/kvm` — только TCP-loopback, поэтому не в
  `integration-test` Docker-таргете. Использует `Daemon::new()` (без
  персистентности) — этот тест про gRPC-слой, не про `andler-store`.
  Включает три теста на `CreateInstance`:
  `create_instance_round_trips_over_real_grpc_and_status_reports_created`
  (полный happy path, плюс `oneof`-вариант `RenderBackend::Venus`),
  `create_instance_with_bridge_network_round_trips_over_real_grpc`
  (`NetworkMode::Bridge` — `oneof`-вариант с данными, не просто
  пустой message-маркер, единственный надёжный способ проверить, что
  `prost` реально переживает protobuf-сериализацию этой ветки по TCP, а
  не только в памяти, как unit-тесты `convert.rs`), и
  `create_instance_missing_cpu_field_round_trips_as_invalid_argument`
  (отсутствие обязательного `Option`-поля -> `INVALID_ARGUMENT`, не паника
  сервиса).

## Требования к окружению

Доступ к `/dev/kvm` (пользователь в группе `kvm`) — без root и без
`CAP_SYS_ADMIN`. См. §10 архитектурного плана.
