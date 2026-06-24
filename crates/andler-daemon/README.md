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
- **Персистентность** — состояние инстансов хранится только в памяти
  процесса (`tokio::sync::RwLock<HashMap<InstanceId, InstanceRecord>>`).
  Перезапуск `andlerd` сейчас теряет весь список инстансов. `andler-store`
  должен будет сохранять `InstanceConfig`/`InstanceState` и восстанавливать
  их при старте — это отдельный шаг, не часть `daemon.rs`.
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
  `create_android_instance`).
- `grpc_roundtrip_test` (`#[cfg(test)]`, не помечен `#[ignore]`) — реальный
  `tonic::transport::Server` на эфемерном `127.0.0.1`-порту + реальный
  `AndlerServiceClient` через настоящий TCP. Проверяет то, что unit-тесты
  `andler-rpc::convert` не могут: что весь путь "клиент сериализует -> сервер
  роутит на метод трейда -> зовёт `Daemon` -> сериализует ответ обратно"
  реально работает по сети, а не только компилируется. Не требует
  `qemu-img`/`/dev/kvm` — только TCP-loopback, поэтому не в
  `integration-test` Docker-таргете.

## Требования к окружению

Доступ к `/dev/kvm` (пользователь в группе `kvm`) — без root и без
`CAP_SYS_ADMIN`. См. §10 архитектурного плана.
