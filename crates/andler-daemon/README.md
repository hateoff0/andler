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

## Что здесь НЕ реализовано (следующие слои поверх `Daemon`)

- **Сеть/gRPC** — `Daemon` ничего не знает про `andler-rpc`/протокол. Когда
  `andler-rpc` будет готов, появится `service.rs`, реализующий gRPC-сервис
  как тонкую обёртку поверх методов `Daemon`.
- **Персистентность** — состояние инстансов хранится только в памяти
  процесса (`tokio::sync::RwLock<HashMap<InstanceId, InstanceRecord>>`).
  Перезапуск `andlerd` сейчас теряет весь список инстансов. `andler-store`
  должен будет сохранять `InstanceConfig`/`InstanceState` и восстанавливать
  их при старте — это отдельный шаг, не часть `daemon.rs`.
- **Резолв `AndroidProfile -> InstanceConfig`** (проверка кэша базового
  образа, скачивание, создание overlay-диска через `andler-disk`,
  см. §4.4.2 архитектурного плана) — `Daemon::create_instance` принимает
  уже готовый `InstanceConfig`, не `AndroidProfile`; резолв происходит
  раньше вызова `create_instance`, и эта логика пока не написана.

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

## Требования к окружению

Доступ к `/dev/kvm` (пользователь в группе `kvm`) — без root и без
`CAP_SYS_ADMIN`. См. §10 архитектурного плана.
