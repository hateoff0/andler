# andler-store

Персистентное хранилище состояния инстансов на SQLite (`rusqlite`,
feature `bundled` — собственная статически слинкованная копия
`libsqlite3`, без зависимости от системной библиотеки).

## Что здесь есть

- `store.rs` — **реализовано**. `Store` — репозиторий с двумя таблицами:
  - `instances` (`id TEXT PRIMARY KEY`, `config_json TEXT`, `state_json
    TEXT`): `InstanceConfig` и `InstanceState` хранятся как JSON-блобы, а не
    как набор типизированных колонок — единственные паттерны доступа на
    сегодня — "всё по `InstanceId`" и "всё для восстановления при старте",
    фильтрации по отдельным полям конфигурации нет.
  - `snapshots` (`id TEXT PRIMARY KEY`, `instance_id TEXT NOT NULL`,
    `tag TEXT NOT NULL`, `description TEXT`, `created_at TEXT NOT NULL`):
    метаданные снапшотов (сами данные хранятся внутри qcow2-файлов).
    UNIQUE INDEX на `(instance_id, tag)`, `ON DELETE CASCADE` при удалении
    инстанса.
  - `Store::open(path)` — sqlite-файл на диске, для `andlerd` в проде.
  - `Store::open_in_memory()` — временная in-memory база, только для
    тестов (см. §7.1 `CORE_ARCHITECTURE_PLAN.md`).
  - `save_instance(cfg, state)` — `INSERT OR REPLACE`, та же семантика
    "перезапись по id", что и у `Daemon::create_instance`.
  - `save_state(id, state)` — обновляет только `state_json`, не трогая
    `config_json`; основной путь вызова — после каждого перехода FSM.
    Возвращает `StoreError::NotFound`, если записи нет (в отличие от
    `save_instance` — здесь это не штатный "create or replace").
  - `load_instance(id)` / `load_all()` — чтение одной записи или всех
    (восстановление списка инстансов при старте `andlerd`).
  - `delete_instance(id)` — идемпотентно, удаление отсутствующей записи
    не ошибка; каскадно удаляет связанные снапшоты.
  - `save_snapshot(snapshot)` / `load_snapshots(instance_id)` /
    `get_snapshot(instance_id, tag)` / `delete_snapshot(instance_id, tag)` —
    CRUD для метаданных снапшотов.
- `error.rs` — `StoreError`: `NotFound`, `Sqlite` (прозрачно из
  `rusqlite::Error`), `Serde` (из `serde_json::Error`), `TaskJoin` (из
  `tokio::task::JoinError` — для `spawn_blocking`).

## Важно

Этот крейт не хранит бизнес-логику переходов состояний — только
персистентность того, что уже решено в `andler-core`. Валидация и переходы
FSM остаются в `andler-core`/`andler-daemon`. `Store` не знает про
`HypervisorBackend`/backend-хэндлы — это намеренно вне его ответственности
(хэндл процесса не переживает перезапуск демона в любом случае, его не
имеет смысла персистировать).

## Конкурентность

Одно `rusqlite::Connection`, под `Arc<Mutex<...>>`, каждый вызов уходит в
`tokio::task::spawn_blocking`. При текущей частоте операций (переходы FSM
по одному инстансу, не сотни в секунду) отдельный writer-поток или
WAL-режим не оправдывают сложности — см. комментарий в `store.rs`.

## Что здесь НЕ реализовано (следующие шаги)

- **Реальные миграции схемы** — на сегодня две таблицы с JSON-колонками,
  поэтому `CREATE TABLE IF NOT EXISTS` достаточно. Понадобится настоящий
  миграционный механизм, только если появится типизированная (не JSON)
  колонка для индексации/фильтрации.
- **Удаление инстанса end-to-end** — `delete_instance` есть в `Store`, но
  `Daemon` пока не имеет метода удаления инстанса вообще (см. TODO в
  `daemon.rs` про Factory Reset). Snapshot-операции уже реализованы.
- **WAL-режим/отдельный writer-поток** — не нужны при текущей частоте
  операций (см. "Конкурентность" выше); пересмотреть, если появится
  заметная конкуренция за один `Mutex<Connection>`.

См. README `andler-daemon` — `Daemon::with_store`/`Daemon::restore` уже
используют этот крейт для персистентности и восстановления при
перезапуске.

## Тесты

`store::tests` — 18 тестов на `Store::open_in_memory()`: round-trip
конфигурации и состояния (включая `InstanceState::Error { message }` —
единственный вариант с полем), перезапись существующего id, частичное
обновление состояния, `NotFound` для отсутствующих записей,
`load_all`/`delete_instance` на пустом и непустом хранилище, плюс два
теста на вспомогательный парсинг `InstanceId` из строки, плюс 7 тестов
на snapshot CRUD (round-trip, уникальность тега, каскадное удаление,
get/delete, verschiedene InstanceId для одного тега).

Не требует `qemu-img`/`/dev/kvm`/сети — только sqlite `:memory:`, поэтому
без `#[ignore]`, гоняется в обычном unit-test таргете.
