# План декомпозиции Andler

## Цель

Уменьшить размер крупнейших файлов проекта, сохранив компиляцию и API на каждом шаге.
Основной кандидат — `daemon/src/daemon.rs` (3209 строк, 53% — тесты).

---

## Статус

| Шаг | Описание | Статус |
|-----|----------|--------|
| 1 | Вынести тесты из daemon.rs | |
| 2 | Вынести error.rs и types.rs | |
| 3 | Вынести snapshot_ops.rs и query_ops.rs | |
| 4 | Вынести instance_ops.rs и clone_ops.rs | |
| 5 | Разбить тесты по модулям | |
| Финал | Проверка: cargo test --workspace (лучше через docker смотреть docker/README.md) | |

---

## Шаг 1: Вынос тестов из daemon.rs

**Цель:** Уменьшить daemon.rs с 3209 до ~1520 строк.
**Риск:** Минимальный. Тесты не меняют публичный API.

### Что делаем

1. Создаём `daemon/src/tests/mod.rs`
2. Переносим туда **весь** блок `#[cfg(test)] mod tests { ... }` (строки 1522–3209)
3. В начале нового файла: `use super::*;` (доступ к приватному коду Daemon)
4. Переносим все `use`-импорты из тестового модуля
5. В `daemon.rs` остаётся только:

```rust
#[cfg(test)]
mod tests;
```

### Структура файлов после шага

```
daemon/src/
├── daemon.rs        ← ~1520 строк (логика без тестов)
└── tests/
    └── mod.rs       ← ~1688 строк (все тесты + хелперы)
```

### Проверка

```bash
cargo test -p daemon
```

---

## Шаг 2: Вынос типов и ошибок

**Цель:** Вырезать из daemon.rs строки 31–248 (~220 строк).
**Риск:** Низкий. Типы не зависят от методов Daemon.

### Что делаем

1. Создаём `daemon/src/error.rs` — переносим `DaemonError` (строки 31–138)
2. Создаём `daemon/src/types.rs` — переносим:
   - `InstanceRecord` (строки 156–160)
   - `SnapshotRecord` (строки 165–172)
   - `InstanceDirGuard` + `new()` + `disarm()` + `impl Drop` (строки 217–248)
   - `purge_instance_files()` (строки 261–289)
   - `InstanceSummary` (строки 1509–1514)

3. В `daemon.rs` добавляем в начало:

```rust
mod error;
mod types;

pub use error::DaemonError;
pub(crate) use types::{
    InstanceDirGuard, InstanceRecord, InstanceSummary, SnapshotRecord,
    purge_instance_files,
};
```

4. Все `use crate::DaemonError` и `use crate::types::*` внутри daemon.rs продолжают работать благодаря re-export.

### Импорты в error.rs

```rust
use andler_core::{BackendError, InstanceState};
use thiserror::Error;
```

### Импорты в types.rs

```rust
use std::path::PathBuf;
use andler_core::{BackendHandle, InstanceConfig, InstanceId, InstanceState};
use andler_core::BackendStatus;
```

### Структура файлов после шага

```
daemon/src/
├── daemon.rs        ← ~1300 строк
├── error.rs         ← ~110 строк
├── types.rs         ← ~110 строк
└── tests/
    └── mod.rs       ← ~1688 строк
```

### Проверка

```bash
cargo check -p daemon
cargo test -p daemon
```

---

## Шаг 3: Вынос snapshot_ops.rs и query_ops.rs

**Цель:** Вырезать из daemon.rs методы снапшотов и запросов.
**Риск:** Средний. Методы используют `&self` и обращаются к `self.instances`, `self.store`, `self.backends`.

### Что делаем

1. Создаём `daemon/src/snapshot_ops.rs` — переносим блок `impl Daemon` с методами:
   - `create_snapshot()` (строки 835–895)
   - `restore_snapshot()` (строки 902–939)
   - `delete_snapshot()` (строки 944–988)
   - `list_snapshots()` (строки 995–1053)

2. Создаём `daemon/src/query_ops.rs` — переносим блок `impl Daemon` с методами:
   - `status()` (строки 1226–1242)
   - `stream_instance_logs()` (строки 1265–1296)
   - `stream_resource_metrics()` (строки 1305–1326)
   - `list_instances()` (строки 1348–1358)
   - `get_instance_config()` (строки 1494–1500)

3. В `daemon.rs` добавляем:

```rust
mod snapshot_ops;
mod query_ops;
```

4. Импорты в каждом ops-файле:

```rust
// snapshot_ops.rs
use crate::Daemon;
use crate::error::DaemonError;
use crate::types::{InstanceRecord, SnapshotRecord};
use andler_core::{BackendKind, CloneMode, HypervisorBackend, InstanceId, InstanceState};

// query_ops.rs
use crate::Daemon;
use crate::error::DaemonError;
use crate::types::InstanceSummary;
use andler_core::{InstanceId, InstanceState, LogLine, ResourceMetrics};
use futures_core::stream::BoxStream;
```

### Важно

Все методы в snapshot_ops.rs и query_ops.rs — это `impl Daemon { pub async fn ... }`.
Rust позволяет иметь множественные `impl` блоки для одного типа в разных файлах того же модуля.

### Структура файлов после шага

```
daemon/src/
├── daemon.rs        ← ~800 строк
├── error.rs         ← ~110 строк
├── types.rs         ← ~110 строк
├── snapshot_ops.rs  ← ~220 строк
├── query_ops.rs     ← ~280 строк
└── tests/
    └── mod.rs       ← ~1688 строк
```

### Проверка

```bash
cargo check -p daemon
cargo test -p daemon
```

---

## Шаг 4: Вынос instance_ops.rs и clone_ops.rs

**Цель:** Вырезать оставшуюся логику из daemon.rs.
**Риск:** Высокий. Эти методы используют `&mut self` (через RwLock), вызывают `self.persist_*`, создают директории, работают с filesystem.

### Что делаем

1. Создаём `daemon/src/instance_ops.rs` — переносим:
   - `create_instance()` (строки 466–484)
   - `create_android_instance()` (строки 525–597)
   - `start_instance()` (строки 1073–1121)
   - `stop_instance()` (строки 1131–1175)
   - `pause_instance()` (строки 1188–1201)
   - `resume_instance()` (строки 1206–1219)
   - `remove_instance()` (строки 1429–1478)

2. Создаём `daemon/src/clone_ops.rs` — переносим:
   - `clone_instance()` (строки 630–715)
   - `export_instance_disk()` (строки 740–750)
   - `terminal_clonable_instance_config()` (строки 762–778)
   - `find_live_clones()` (строки 805–821)

3. В `daemon.rs` добавляем:

```rust
mod instance_ops;
mod clone_ops;
```

4. Импорты в clone_ops.rs (самый сложный — использует `andler_disk::clone::*`):

```rust
use crate::Daemon;
use crate::error::DaemonError;
use crate::types::{InstanceDirGuard, InstanceRecord};
use andler_core::{
    BackendKind, CloneMode, HypervisorBackend, InstanceConfig, InstanceId, InstanceKind,
    InstanceState,
};
use std::path::PathBuf;
use std::sync::Arc;
```

### Структура файлов после шага

```
daemon/src/
├── daemon.rs        ← ~250 строк (ядро: struct, constructors, persist helpers)
├── error.rs         ← ~110 строк
├── types.rs         ← ~110 строк
├── snapshot_ops.rs  ← ~220 строк
├── query_ops.rs     ← ~280 строк
├── instance_ops.rs  ← ~400 строк
├── clone_ops.rs     ← ~250 строк
└── tests/
    └── mod.rs       ← ~1688 строк
```

### Проверка

```bash
cargo check -p daemon
cargo test -p daemon
```

---

## Шаг 5: Разбивка тестов по модулям

**Цель:** Разнести 53 теста из `tests/mod.rs` по доменным файлам.
**Риск:** Минимальный. Это чисто структурное изменение.

### Распределение тестов

#### `tests/helpers.rs` (~50 строк)
- `TestTempDir` (struct + impl + Drop)
- `sample_config()`
- `sample_android_config()`

#### `tests/instance_tests.rs` (~350 строк)
Тесты жизненного цикла инстансов:
- `create_instance_registers_with_created_state`
- `status_on_unknown_instance_returns_instance_not_found`
- `start_instance_rejects_passthrough_via_backend_validation`
- `pause_before_start_returns_handle_not_found`
- `pause_on_unknown_instance_returns_instance_not_found`
- `resume_before_start_returns_handle_not_found`
- `resume_on_unknown_instance_returns_instance_not_found`
- `stop_before_start_returns_handle_not_found`
- `double_create_with_same_id_overwrites_record`
- `create_android_instance_resolves_profile_and_creates_overlay` (#[ignore])
- `create_android_instance_fails_when_base_image_missing`
- `create_android_instance_cleans_up_instance_dir_on_missing_ovmf_template`
- `remove_instance_on_unknown_instance_returns_instance_not_found`
- `remove_instance_succeeds_from_created`
- `remove_instance_succeeds_from_error_state`
- `remove_instance_rejects_running_instance`
- `remove_instance_rejects_every_non_terminal_state`
- `remove_instance_succeeds_from_stopped`
- `remove_instance_also_deletes_from_store`
- `remove_instance_disappears_from_list_instances`
- `remove_instance_without_purge_leaves_disk_and_firmware_files`
- `remove_instance_with_purge_deletes_disk_and_firmware_files`
- `remove_instance_with_purge_keeps_non_empty_parent_directory`
- `remove_instance_with_purge_never_deletes_shared_base_image_or_ovmf_code`
- `remove_instance_with_purge_tolerates_already_missing_files`

#### `tests/clone_tests.rs` (~350 строк)
Тесты клонирования и экспорта:
- `clone_on_unknown_instance_returns_instance_not_found`
- `clone_rejects_linux_vm_with_shared_base`
- `clone_rejects_non_terminal_source_state`
- `clone_instance_with_linked_mode_creates_overlay_pointing_at_source_disk` (#[ignore])
- `clone_instance_with_full_standalone_mode_has_no_base_image` (#[ignore])
- `clone_instance_with_shared_base_mode_survives_source_purge` (#[ignore])
- `clone_instance_of_a_clone_is_allowed` (#[ignore])
- `clone_linux_vm_linked_mode_is_allowed`
- `clone_linux_vm_full_standalone_mode_is_allowed`
- `clone_linux_vm_shared_base_mode_returns_shared_base_not_supported`
- `clone_linux_vm_rejects_non_terminal_source_state`
- `export_linux_vm_disk_creates_standalone_file` (#[ignore])
- `export_linux_vm_disk_does_not_block_on_instance_kind`
- `export_instance_disk_creates_standalone_file_without_registering_instance` (#[ignore])
- `find_live_clones_on_unknown_instance_returns_instance_not_found`
- `find_live_clones_is_empty_when_nothing_references_the_disk`
- `find_live_clones_finds_instance_whose_base_image_is_the_source_disk`
- `find_live_clones_ignores_instances_sharing_only_the_base_image`
- `remove_instance_with_purge_rejects_when_live_linked_clone_exists`
- `remove_instance_with_purge_succeeds_when_clone_is_full_standalone`

#### `tests/snapshot_tests.rs` (~20 строк)
Тесты снапшотов (если есть unit-тесты):
- `create_instance_respects_snapshot_timeout_per_operation_override`
- `delete_snapshot_removes_snapshot_from_backend_and_store`
- `delete_snapshot_not_found_returns_error`
- `list_snapshots_merges_store_and_backend_results`
- `snapshot_timeout_per_operation_overrides_instance_default`

#### `tests/query_tests.rs` (~200 строк)
Тесты запросов и стриминга:
- `stream_logs_before_start_returns_empty_stream_not_error`
- `stream_logs_on_unknown_instance_returns_instance_not_found`
- `list_instances_on_empty_daemon_returns_empty_vec`
- `list_instances_returns_one_summary_per_created_instance`
- `list_instances_reflects_state_after_failed_start`
- `list_instances_after_restore_includes_restored_instances`
- `get_instance_config_on_unknown_instance_returns_instance_not_found`
- `get_instance_config_returns_full_config_unchanged`
- `get_instance_config_reflects_current_record_not_a_stale_snapshot`

#### `tests/persistence_tests.rs` (~200 строк)
Тесты персистентности (Store + restore):
- `with_store_persists_created_instance`
- `with_store_persists_failed_start_as_error_state`
- `daemon_without_store_does_not_panic_on_state_transitions`
- `restore_recreates_daemon_from_store_contents`
- `restore_marks_running_instance_as_error_since_handle_is_lost`
- `restore_marks_every_non_terminal_state_as_error`
- `restore_keeps_terminal_states_unchanged`
- `restore_on_empty_store_yields_daemon_with_no_instances`
- `restored_daemon_continues_to_persist_further_transitions`

### tests/mod.rs после разбивки

```rust
mod helpers;
mod instance_tests;
mod clone_tests;
mod snapshot_tests;
mod query_tests;
mod persistence_tests;
```

### Проверка

```bash
cargo test -p daemon
```

---

## Итоговая структура проекта

```
daemon/src/
├── lib.rs              ← mod declarations + re-exports
├── daemon.rs           ← struct Daemon, constructors, persist helpers (~250 строк)
├── error.rs            ← DaemonError enum (~110 строк)
├── types.rs            ← InstanceRecord, SnapshotRecord, InstanceSummary, InstanceDirGuard (~110 строк)
├── instance_ops.rs     ← create, start, stop, pause, resume, remove (~400 строк)
├── clone_ops.rs        ← clone, export, find_live_clones (~250 строк)
├── snapshot_ops.rs     ← create/restore/delete/list snapshots (~220 строк)
├── query_ops.rs        ← status, list, get_config, stream (~280 строк)
├── service.rs          ← gRPC service handlers (без изменений)
├── grpc_roundtrip_test.rs ← integration tests (без изменений)
└── tests/
    ├── mod.rs          ← mod declarations (~10 строк)
    ├── helpers.rs      ← TestTempDir, sample configs (~50 строк)
    ├── instance_tests.rs   (~350 строк)
    ├── clone_tests.rs      (~350 строк)
    ├── snapshot_tests.rs   (~20 строк)
    ├── query_tests.rs      (~200 строк)
    └── persistence_tests.rs (~200 строк)
```

### Сравнение "до" и "после"

| Файл | До | После |
|------|-----|-------|
| daemon.rs | 3209 | ~250 |
| error.rs | — | ~110 |
| types.rs | — | ~110 |
| instance_ops.rs | — | ~400 |
| clone_ops.rs | — | ~250 |
| snapshot_ops.rs | — | ~220 |
| query_ops.rs | — | ~280 |
| tests/mod.rs | — | ~10 |
| tests/helpers.rs | — | ~50 |
| tests/instance_tests.rs | — | ~350 |
| tests/clone_tests.rs | — | ~350 |
| tests/snapshot_tests.rs | — | ~20 |
| tests/query_tests.rs | — | ~200 |
| tests/persistence_tests.rs | — | ~200 |
| **ИТОГО** | **3209** | **~2800** |

Общий размер вырастает на ~400 строк из-за дублирования imports и mod declarations.
Но каждый файл теперь < 450 строк, и логика отделена от тестов.

---

## Золотые правила декомпозиции

### 1. Тесты выносятся ПЕРВЫМ
Это самый безопасный шаг. Не меняет публичный API, не ломает компиляцию.

### 2. `impl Trait` в разных файлах — это OK
Rust позволяет иметь множественные `impl Daemon` в разных файлах одного модуля.

### 3. Не трогай файлы < 500 строк
Если файл уже чисто структурирован — оставь в покости.

### 4. Декомпозиция по ДОМЕНУ, а не по техническому слою
Правильно: `instance_ops.rs`, `clone_ops.rs`, `snapshot_ops.rs`.
Неправильно: `errors.rs`, `helpers.rs`, `types.rs`.

### 5. `main.rs` — это всегда диспетчер
Если `main()` > 100 строк — она слишком большая.

### 6. Не создавай файлы < 50 строк
Минимум 80–100 строк на файл.

### 7. Re-export для обратной совместимости
После выноса модуля, сделай re-export в родительском модуле.

### 8. Декомпозиция = итеративный процесс
После каждого шага — `cargo test --workspace`.

---

## Файлы, которые НЕЛЬЗЯ дробить

| Файл | Строк | Причина |
|------|-------|---------|
| `convert.rs` | 1200 | Плоские From/TryFrom impl'ы, каждый 5–20 строк. Дробление = 10+ файлов по 60 строк без смысла |
| `grpc_roundtrip_test.rs` | 810 | Интеграционные тесты. Единый фикстурный паттерн, независимые тесты |
| `qmp.rs` | 845 | QMP клиент — единый протокол. 47% файла — тесты |
| `backend.rs` | 751 | HypervisorBackend impl. Max метод 62 строки — допустимо |
| `store.rs` | 826 | Store CRUD. Max метод 38 строк. Только если планируется рост |
| `cli/main.rs` | 1185 | Кандидат на декомпозицию (P1), но после daemon.rs |
