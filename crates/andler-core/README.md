# andler-core

Доменная модель проекта: типы конфигурации инстансов, конечный автомат состояний
(`fsm.rs`) и абстракция backend'а гипервизора (`backend.rs`).

## Что здесь есть

- `backend.rs` — trait `HypervisorBackend`, который реализуют `andler-qemu` и (пока
  пустой) `andler-vmm`. Любой метод, не реализованный конкретным backend'ом, обязан
  возвращать `BackendError::NotImplemented`, а не паниковать — см.
  `docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §2.1.
- `fsm.rs` — состояния инстанса (`Created → Starting → Running ⇄ Paused → Stopping →
  Stopped`, плюс `Error { message }`) и разрешённые переходы между ними. Набор
  состояний соответствует §4.1 `CORE_ARCHITECTURE_PLAN.md`.
- `config/` — структуры `InstanceConfig` и всё, из чего он состоит, по одному файлу
  на блок ресурсов: `cpu.rs`, `memory.rs`, `gpu.rs` (включая `RenderBackend`), `disk.rs`
  (включая overlay-диски для Android, см. `DiskConfig::overlay`), `network.rs`,
  `display.rs`, и `instance.rs`, который собирает их в `InstanceConfig` вместе с
  `InstanceId`/`InstanceKind`. Это чистые данные без побочных эффектов — каждый файл
  также даёт `reference_default()`, соответствующий `scripts/start.sh`.
- `android_profile.rs` — `AndroidProfile` и его резолв в `InstanceConfig` (overlay-диск
  над базовым образом). См. §4.4 архитектурного плана.
- `error.rs` — общие типы ошибок домена.

## Что здесь НЕ должно появляться

- Никакого прямого вызова `qemu-system-x86_64` или работы с файловой системой — это
  `andler-qemu` и `andler-disk`.
- Никакого gRPC/сериализации протокола — это `andler-rpc`.
- Никакого SQL — это `andler-store`.

`andler-core` не должен зависеть ни от одного другого крейта workspace'а — он
самый нижний слой, и любая попытка добавить зависимость "вверх" (на `andler-qemu`,
`andler-rpc` и т.п.) — признак того, что код лежит не на своём месте.

## Тесты

Полностью юнит-тестируемый без QEMU и без `/dev/kvm` — это весь смысл выноса домена
в отдельный крейт. Если тест в `andler-core` требует реального процесса QEMU — скорее
всего, тест попал не в тот крейт.

## Связанная документация

`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, разделы 2–4.
