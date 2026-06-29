# План декомпозиции Andler

## Цель

Уменьшить размер крупнейших файлов проекта, сохранив компиляцию и API на каждом шаге.
Основной кандидат — `daemon/src/daemon.rs` (3209 строк, 53% — тесты).

---

## Статус

| Шаг | Описание | Статус |
|-----|----------|--------|
| 1 | Вынести тесты из daemon.rs | ✅ |
| 2 | Вынести error.rs и types.rs | ✅ |
| 3 | Вынести snapshot_ops.rs и query_ops.rs | ✅ |
| 4 | Вынести instance_ops.rs и clone_ops.rs | ✅ |
| 5 | Разбить тесты по модулям | ✅ |
| Финал | Проверка: docker compose run --rm unit-test | ✅ |

**Декомпозиция завершена.** `daemon.rs` (3209 строк) → `daemon/mod.rs` (250 строк, 92% reduction).

---

## Итоговая структура

```
daemon/src/daemon/
├── mod.rs            ← struct Daemon, constructors, backend_for, persist helpers (~250 строк)
├── error.rs          ← DaemonError enum, 14 variants (~113 строк)
├── types.rs          ← InstanceRecord, SnapshotRecord, InstanceDirGuard, InstanceSummary (~129 строк)
├── instance_ops.rs   ← create, create_android, start, stop, pause, resume, remove (~437 строк)
├── clone_ops.rs      ← clone_instance, export_instance_disk, find_live_clones (~189 строк)
├── snapshot_ops.rs   ← create/restore/delete/list snapshots (~234 строк)
├── query_ops.rs      ← status, log/metrics streaming, list_instances, get_instance_config (~161 строк)
└── tests/
    ├── mod.rs          ← mod declarations + use super::* (~12 строк)
    ├── common.rs       ← TestTempDir, sample_config, sample_android_config (~67 строк)
    ├── create.rs       ← create instance tests (~183 строк)
    ├── status.rs       ← status/stream tests (~36 строк)
    ├── start_stop.rs   ← start/pause/resume/stop tests (~79 строк)
    ├── persistence.rs  ← restore/with_store tests (~161 строк)
    ├── remove.rs       ← remove/purge tests (~244 строк)
    ├── clone.rs        ← clone/export/find_live_clones tests (~681 строк)
    └── list_config.rs  ← list/get_config tests (~101 строк)
```

### Сравнение "до" и "после"

| Файл | До | После |
|------|-----|-------|
| daemon.rs → mod.rs | 3209 | 250 |
| error.rs | — | 113 |
| types.rs | — | 129 |
| instance_ops.rs | — | 437 |
| clone_ops.rs | — | 189 |
| snapshot_ops.rs | — | 234 |
| query_ops.rs | — | 161 |
| tests/ (9 файлов) | — | 1500 |

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
После каждого шага — `docker compose -f docker/docker-compose.yml run --rm unit-test`.

### 9. Действуй пошагово, не разом
Каждый шаг выполняется **по отдельности**, не параллельно. Порядок:
1. Сделал шаг (вырезал код, создал файлы)
2. Проверил компиляцию (`docker compose -f docker/docker-compose.yml build --no-cache unit-test && docker compose -f docker/docker-compose.yml run --rm unit-test`)
3. Проверил вручную — перечитал вырезанный код и новый файл, убедился что:
   - Все методы/типы на месте (пропущенных нет)
   - Импорты полные (ничего не забыли)
   - Re-export работает (родительский модуль экспортирует всё нужное)
4. Если шаг включает тесты — перечитай каждый тест и убедись что:
   - Тест остался в том же модуле, что и раньше (или правильном подмодуле)
   - Все `use super::*` / `use crate::*` работают
   - Не потерялись `#[ignore]` и `#[cfg]` атрибуты
5. Только после этого — переходи к следующему шагу

**Никогда не объединяй два шага в один коммит/проверку.** Если шаг 2 и шаг 3 кажутся связанными — всё равно делай их отдельно, с отдельной проверкой.

### Чеклист ручной проверки после каждого шага

- [ ] Все вырезанные методы/типы присутствуют в новом файле
- [ ] Импорты нового файла полные (сверь с оригиналом)
- [ ] Re-export в родительском модуле работает (`pub use` / `pub(crate) use`)
- [ ] Тесты (если есть) не потеряли `#[ignore]` и `#[cfg]` атрибуты
- [ ] `docker compose -f docker/docker-compose.yml build --no-cache unit-test && docker compose -f docker/docker-compose.yml run --rm unit-test` проходят
- [ ] Прочитай diff — убедись что не удалил лишнего

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

---

## Завершение

Декомпозиция `daemon/src/daemon.rs` завершена. Все 5 шагов выполнены:

| Шаг | Описание | Статус |
|-----|----------|--------|
| 1 | Вынос тестов из daemon.rs | ✅ |
| 2 | Вынести error.rs и types.rs | ✅ |
| 3 | Вынести snapshot_ops.rs и query_ops.rs | ✅ |
| 4 | Вынести instance_ops.rs и clone_ops.rs | ✅ |
| 5 | Разбить тесты по модулям | ✅ |

**Итог:**
- `daemon.rs` (3209 строк) → `daemon/mod.rs` (250 строк, 92% reduction)
- Docker: 80 passed, 0 failed, 7 ignored
- Все тесты проходят, компиляция чистая
