//! Репозиторий `InstanceConfig`/`InstanceState` на sqlite.
//!
//! Схема — одна таблица `instances` с двумя JSON-колонками
//! (`config_json`, `state_json`), а не нормализованный набор колонок на
//! каждое поле `InstanceConfig`: `InstanceConfig` уже `Serialize`/
//! `Deserialize` целиком (см. `andler_core::config::instance`), схема
//! активно меняется на этом этапе проекта, и ничего в `andler-store` не
//! фильтрует/не сортирует по отдельным полям конфигурации — единственный
//! паттерн доступа — "дай мне всё по этому `InstanceId`" или "дай мне все
//! записи на старте демона". `state_json` хранится отдельно от
//! `config_json`, а не как часть одного блоба, потому что обновляется
//! значительно чаще (каждый переход FSM) и независимо от конфигурации —
//! `save_state` не должен требовать пере-сериализации всего конфига.
//!
//! Конкурентный доступ: `rusqlite::Connection` не `Sync`, поэтому делимся
//! одним соединением между вызовами через `Arc<Mutex<Connection>>` —
//! sqlite сам по себе не обслуживает параллельные записи лучше одного
//! соединения с сериализованным доступом (busy_timeout/WAL дали бы
//! параллелизм read/write, но при текущем объёме операций — несколько
//! изменений FSM в секунду на инстанс — отдельный writer-поток или WAL не
//! окупают добавленной сложности; см. README, "Куда дальше").
//!
//! Каждый публичный метод — `async fn`, который уходит в
//! `tokio::task::spawn_blocking`: rusqlite — блокирующий API, и держать
//! tokio-воркер занятым на время диска было бы неправильно даже при
//! низкой частоте вызовов.

use std::path::Path;
use std::sync::{Arc, Mutex};

use andler_core::{InstanceConfig, InstanceId, InstanceState};
use rusqlite::Connection;
use uuid::Uuid;

use crate::error::StoreError;

/// Персистентный репозиторий инстансов поверх sqlite.
///
/// `Clone` — дешёвый (это просто клон `Arc`), что позволяет передавать
/// `Store` в несколько мест `andler-daemon` (например, и в `Daemon`, и в
/// фоновую задачу восстановления при старте) без дополнительной обёртки в
/// `Arc` на стороне вызывающего.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

/// Одна строка из таблицы `instances` целиком — то, что нужно
/// `andler-daemon`, чтобы восстановить `InstanceRecord` при старте: и
/// конфигурация, и последнее известное состояние FSM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInstance {
    pub config: InstanceConfig,
    pub state: InstanceState,
}

/// Метаданные снапшота, хранящиеся в таблице `snapshots`.
/// Сам снапшот (данные диска + состояние CPU/памяти) хранится внутри
/// qcow2-файла — здесь только метаданные для быстрого доступа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub id: uuid::Uuid,
    pub instance_id: InstanceId,
    pub tag: String,
    pub description: Option<String>,
    pub created_at: String,
}

impl Store {
    /// Открывает (или создаёт, если файла ещё нет) sqlite-базу по
    /// указанному пути и применяет схему. Путь, а не строка подключения —
    /// `andler-daemon` всегда работает с конкретным файлом на диске
    /// (`/var/lib/andler/state.db` в проде), и `Path` явно отражает это в
    /// сигнатуре, в отличие от произвольной sqlite connection-строки,
    /// которая могла бы случайно указать на `:memory:` или сетевую базу,
    /// которые `andler-store` не поддерживает и не тестирует.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, StoreError> {
            let conn = Connection::open(path)?;
            apply_schema(&conn)?;
            Ok(conn)
        })
        .await??;

        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Открывает временную in-memory базу. Только для тестов — см.
    /// `docs/architecture/CORE_ARCHITECTURE_PLAN.md` §7.1
    /// ("Миграции и CRUD на временной/in-memory sqlite"): in-memory база
    /// существует ровно столько, сколько живёт `Connection`, и закрытие
    /// последнего владельца `Store` уничтожает данные — ожидаемо и
    /// нужно только здесь, не в `open`.
    pub async fn open_in_memory() -> Result<Self, StoreError> {
        let conn = tokio::task::spawn_blocking(|| -> Result<Connection, StoreError> {
            let conn = Connection::open_in_memory()?;
            apply_schema(&conn)?;
            Ok(conn)
        })
        .await??;

        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Сохраняет новую запись или полностью перезаписывает существующую с
    /// тем же `InstanceId` (`INSERT OR REPLACE`). Соответствует семантике
    /// `Daemon::create_instance`, которая тоже безусловно перезаписывает
    /// запись с уже существующим `id` (см. комментарий к
    /// `double_create_with_same_id_overwrites_record` в `daemon.rs`) —
    /// `andler-store` не вводит здесь более строгую инвариантность, чем
    /// уже принята слоем выше.
    ///
    /// Записывает `InstanceState::Created` неявно как часть `cfg`? Нет —
    /// состояние передаётся отдельным параметром, а не выводится из
    /// конфигурации, потому что `Store` не имеет (и не должен иметь)
    /// мнения о том, какое состояние корректно для только что
    /// сконструированного `InstanceConfig`; это решение FSM/`Daemon`.
    pub async fn save_instance(
        &self,
        cfg: &InstanceConfig,
        state: &InstanceState,
    ) -> Result<(), StoreError> {
        let id = cfg.id;
        let config_json = serde_json::to_string(cfg)?;
        let state_json = serde_json::to_string(state)?;
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            conn.execute(
                "INSERT OR REPLACE INTO instances (id, config_json, state_json) \
                 VALUES (?1, ?2, ?3)",
                (id.0.to_string(), config_json, state_json),
            )?;
            Ok(())
        })
        .await??;

        Ok(())
    }

    /// Обновляет только `state_json` существующей записи — не трогает
    /// `config_json`. Основной путь вызова: после каждого перехода FSM в
    /// `Daemon` (`start_instance`/`stop_instance`/...), где конфигурация
    /// инстанса не меняется, а меняться (часто) должно только состояние.
    /// Использовать `save_instance` для этого случая означало бы каждый
    /// раз пере-сериализовывать весь `InstanceConfig` без необходимости.
    ///
    /// Возвращает `StoreError::NotFound`, если строки с этим `id` нет —
    /// в отличие от `save_instance`, здесь нет осознанного решения "create
    /// or replace": обновление состояния несуществующей записи означает
    /// рассинхрон между `Daemon` (in-memory) и `Store`, который не должен
    /// маскироваться тихим no-op.
    pub async fn save_state(
        &self,
        id: InstanceId,
        state: &InstanceState,
    ) -> Result<(), StoreError> {
        let state_json = serde_json::to_string(state)?;
        let conn = self.conn.clone();

        let rows_changed = tokio::task::spawn_blocking(move || -> Result<usize, StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            let rows = conn.execute(
                "UPDATE instances SET state_json = ?1 WHERE id = ?2",
                (state_json, id.0.to_string()),
            )?;
            Ok(rows)
        })
        .await??;

        if rows_changed == 0 {
            return Err(StoreError::NotFound(id));
        }

        Ok(())
    }

    /// Возвращает конфигурацию и последнее сохранённое состояние одного
    /// инстанса.
    pub async fn load_instance(&self, id: InstanceId) -> Result<StoredInstance, StoreError> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<StoredInstance, StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            let row = conn
                .query_row(
                    "SELECT config_json, state_json FROM instances WHERE id = ?1",
                    [id.0.to_string()],
                    |row| {
                        let config_json: String = row.get(0)?;
                        let state_json: String = row.get(1)?;
                        Ok((config_json, state_json))
                    },
                )
                .map_err(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(id),
                    other => StoreError::Sqlite(other),
                })?;

            row_to_stored_instance(row)
        })
        .await?
    }

    /// Возвращает все сохранённые инстансы — то, что `andler-daemon`
    /// вызывает один раз при старте, чтобы восстановить
    /// `RwLock<HashMap<InstanceId, InstanceRecord>>` после перезапуска
    /// процесса (см. README `andler-daemon`, раздел "Персистентность").
    /// Порядок строк не гарантируется (нет `ORDER BY`) — вызывающая
    /// сторона кладёт их в `HashMap`, где порядка и так нет.
    pub async fn load_all(&self) -> Result<Vec<StoredInstance>, StoreError> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<Vec<StoredInstance>, StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            let mut stmt = conn.prepare("SELECT config_json, state_json FROM instances")?;
            let rows = stmt.query_map([], |row| {
                let config_json: String = row.get(0)?;
                let state_json: String = row.get(1)?;
                Ok((config_json, state_json))
            })?;

            let mut result = Vec::new();
            for row in rows {
                result.push(row_to_stored_instance(row?)?);
            }
            Ok(result)
        })
        .await?
    }

    /// Удаляет запись инстанса. Идемпотентно — удаление уже отсутствующей
    /// записи не ошибка (в отличие от `save_state`): вызывающая сторона
    /// (например, Factory Reset/удаление инстанса через `Daemon`, см. TODO
    /// в `daemon.rs`) хочет гарантировать "записи нет после вызова", а не
    /// "записи не было до вызова".
    pub async fn delete_instance(&self, id: InstanceId) -> Result<(), StoreError> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            conn.execute("DELETE FROM instances WHERE id = ?1", [id.0.to_string()])?;
            Ok(())
        })
        .await??;

        Ok(())
    }

    // --- Snapshots CRUD ---------------------------------------------------

    /// Сохраняет метаданные снапшота. Вызывается после успешного
    /// `snapshot-save` через QMP — сам снапшот уже записан в qcow2-файл,
    /// здесь только регистрируем метаданные.
    pub async fn save_snapshot(&self, snapshot: &StoredSnapshot) -> Result<(), StoreError> {
        let id = snapshot.id.to_string();
        let instance_id = snapshot.instance_id.0.to_string();
        let tag = snapshot.tag.clone();
        let description = snapshot.description.clone();
        let created_at = snapshot.created_at.clone();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            conn.execute(
                "INSERT OR REPLACE INTO snapshots (id, instance_id, tag, description, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (id, instance_id, tag, description, created_at),
            )?;
            Ok(())
        })
        .await??;

        Ok(())
    }

    /// Возвращает все снапшоты инстанса, отсортированные по дате создания.
    pub async fn load_snapshots(
        &self,
        instance_id: InstanceId,
    ) -> Result<Vec<StoredSnapshot>, StoreError> {
        let instance_id_str = instance_id.0.to_string();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<Vec<StoredSnapshot>, StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            let mut stmt = conn.prepare(
                "SELECT id, instance_id, tag, description, created_at \
                 FROM snapshots WHERE instance_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map([instance_id_str], |row| {
                Ok(StoredSnapshot {
                    id: uuid::Uuid::parse_str(&row.get::<_, String>(0)?)
                        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                    instance_id: InstanceId(
                        uuid::Uuid::parse_str(&row.get::<_, String>(1)?)
                            .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                    ),
                    tag: row.get(2)?,
                    description: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;

            let mut result = Vec::new();
            for row in rows {
                result.push(row?);
            }
            Ok(result)
        })
        .await?
    }

    /// Возвращает один снапшот по тегу.
    pub async fn get_snapshot(
        &self,
        instance_id: InstanceId,
        tag: &str,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let instance_id_str = instance_id.0.to_string();
        let tag = tag.to_string();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<Option<StoredSnapshot>, StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            let result = conn.query_row(
                "SELECT id, instance_id, tag, description, created_at \
                 FROM snapshots WHERE instance_id = ?1 AND tag = ?2",
                (instance_id_str, tag),
                |row| {
                    Ok(StoredSnapshot {
                        id: uuid::Uuid::parse_str(&row.get::<_, String>(0)?)
                            .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                        instance_id: InstanceId(
                            uuid::Uuid::parse_str(&row.get::<_, String>(1)?)
                                .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                        ),
                        tag: row.get(2)?,
                        description: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            );

            match result {
                Ok(snapshot) => Ok(Some(snapshot)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(StoreError::Sqlite(e)),
            }
        })
        .await?
    }

    /// Удаляет снапшот по тегу. Идемпотентно — удаление несуществующего
    /// снапшота не ошибка.
    pub async fn delete_snapshot(
        &self,
        instance_id: InstanceId,
        tag: &str,
    ) -> Result<(), StoreError> {
        let instance_id_str = instance_id.0.to_string();
        let tag = tag.to_string();
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let conn = conn.lock().expect("sqlite connection mutex poisoned");
            conn.execute(
                "DELETE FROM snapshots WHERE instance_id = ?1 AND tag = ?2",
                (instance_id_str, tag),
            )?;
            Ok(())
        })
        .await??;

        Ok(())
    }
}

/// Превращает строку `(config_json, state_json)` в `StoredInstance`,
/// разворачивая обе сериализационные ошибки в `StoreError::Serde`.
/// Вынесено в свободную функцию, а не метод — нужно из трёх разных мест
/// (`load_instance`, `load_all` через `query_map`-замыкание, и могло бы
/// понадобиться в будущих методах фильтрации), и не имеет смысла
/// привязывать к `&self`.
fn row_to_stored_instance(
    (config_json, state_json): (String, String),
) -> Result<StoredInstance, StoreError> {
    let config: InstanceConfig = serde_json::from_str(&config_json)?;
    let state: InstanceState = serde_json::from_str(&state_json)?;
    Ok(StoredInstance { config, state })
}

/// Создаёт таблицу `instances`, если её ещё нет.
///
/// Один `CREATE TABLE IF NOT EXISTS` без отдельного миграционного
/// фреймворка (например, `refinery`/`sqlx::migrate`) — на этом этапе
/// проекта схема — это одна таблица с двумя независимыми от структуры
/// JSON-колонками; добавление нового поля в `InstanceConfig` не требует
/// миграции схемы `andler-store` вообще (это плюс выбранного JSON-блоба).
/// Настоящая миграция понадобится, только если появится колонка с
/// типизированным значением для индексации/фильтрации — тогда это
/// осознанный следующий шаг, не предвосхищаем его сейчас.
fn apply_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS instances (
            id          TEXT PRIMARY KEY,
            config_json TEXT NOT NULL,
            state_json  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS snapshots (
            id          TEXT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            tag         TEXT NOT NULL,
            description TEXT,
            created_at  TEXT NOT NULL,
            FOREIGN KEY (instance_id) REFERENCES instances(id) ON DELETE CASCADE
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_snapshots_instance_tag
            ON snapshots(instance_id, tag);",
    )?;
    Ok(())
}

/// Парсит текстовое представление `InstanceId` (UUID), как оно хранится в
/// колонке `id`. Сейчас не используется напрямую внутри `store.rs`
/// (фильтрация по `id` в SQL всегда идёт через `id.0.to_string()` в
/// обратном направлении), но является естественной парной операцией и
/// тестируется явно — будущий метод, принимающий `id: &str` из внешнего
/// источника (например, CLI-аргумент при ручной инспекции базы), найдёт
/// её здесь, а не будет заново парсить UUID на месте использования.
#[allow(dead_code)]
fn parse_instance_id(raw: &str) -> Result<InstanceId, uuid::Error> {
    Uuid::parse_str(raw).map(InstanceId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig,
        GpuConfig, InputConfig, InstanceKind, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[tokio::test]
    async fn save_and_load_round_trips() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;

        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let loaded = store.load_instance(id).await.unwrap();
        assert_eq!(loaded.config, cfg);
        assert_eq!(loaded.state, InstanceState::Created);
    }

    #[tokio::test]
    async fn load_unknown_instance_returns_not_found() {
        let store = Store::open_in_memory().await.unwrap();
        let err = store.load_instance(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn save_instance_with_existing_id_overwrites() {
        let store = Store::open_in_memory().await.unwrap();
        let mut cfg = sample_config();
        let id = cfg.id;

        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        cfg.name = "renamed".to_string();
        store
            .save_instance(&cfg, &InstanceState::Starting)
            .await
            .unwrap();

        let loaded = store.load_instance(id).await.unwrap();
        assert_eq!(loaded.config.name, "renamed");
        assert_eq!(loaded.state, InstanceState::Starting);
    }

    #[tokio::test]
    async fn save_state_updates_only_state_not_config() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        store
            .save_state(id, &InstanceState::Running)
            .await
            .unwrap();

        let loaded = store.load_instance(id).await.unwrap();
        assert_eq!(loaded.config, cfg);
        assert_eq!(loaded.state, InstanceState::Running);
    }

    #[tokio::test]
    async fn save_state_on_unknown_instance_returns_not_found() {
        let store = Store::open_in_memory().await.unwrap();
        let err = store
            .save_state(InstanceId::new(), &InstanceState::Running)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn load_all_returns_every_saved_instance() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg1 = sample_config();
        let mut cfg2 = sample_config();
        cfg2.name = "second-vm".to_string();

        store
            .save_instance(&cfg1, &InstanceState::Created)
            .await
            .unwrap();
        store
            .save_instance(&cfg2, &InstanceState::Running)
            .await
            .unwrap();

        let mut all = store.load_all().await.unwrap();
        all.sort_by_key(|s| s.config.name.clone());

        assert_eq!(all.len(), 2);
        assert_eq!(all[0].config.name, "second-vm");
        assert_eq!(all[0].state, InstanceState::Running);
        assert_eq!(all[1].config.name, "test-vm");
        assert_eq!(all[1].state, InstanceState::Created);
    }

    #[tokio::test]
    async fn load_all_on_empty_store_returns_empty_vec() {
        let store = Store::open_in_memory().await.unwrap();
        let all = store.load_all().await.unwrap();
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn delete_instance_removes_row() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        store.delete_instance(id).await.unwrap();

        let err = store.load_instance(id).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_unknown_instance_is_not_an_error() {
        let store = Store::open_in_memory().await.unwrap();
        store.delete_instance(InstanceId::new()).await.unwrap();
    }

    #[tokio::test]
    async fn error_state_with_message_round_trips() {
        // Отдельный тест на InstanceState::Error { message } — единственный
        // вариант InstanceState с полем, проверяет, что JSON-сериализация
        // serde для enum с struct-вариантом действительно переживает
        // round-trip через sqlite, а не только через serde_json напрямую
        // (как уже проверено в andler-core::config::instance тестах).
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        let error_state = InstanceState::Error {
            message: "qemu exited with status 1".to_string(),
        };

        store.save_instance(&cfg, &error_state).await.unwrap();
        let loaded = store.load_instance(id).await.unwrap();
        assert_eq!(loaded.state, error_state);
    }

    #[test]
    fn parse_instance_id_round_trips_with_display() {
        let id = InstanceId::new();
        let parsed = parse_instance_id(&id.0.to_string()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_instance_id_rejects_garbage() {
        assert!(parse_instance_id("not-a-uuid").is_err());
    }

    // --- Snapshot CRUD tests -----------------------------------------------

    #[tokio::test]
    async fn save_and_load_snapshot_round_trips() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let instance_id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "backup1".to_string(),
            description: Some("Before update".to_string()),
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        let loaded = store.load_snapshots(instance_id).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tag, "backup1");
        assert_eq!(
            loaded[0].description,
            Some("Before update".to_string())
        );
    }

    #[tokio::test]
    async fn snapshot_unique_tag_per_instance() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let instance_id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let s1 = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "backup".to_string(),
            description: None,
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        store.save_snapshot(&s1).await.unwrap();

        // Same tag, different id — should overwrite (INSERT OR REPLACE)
        let s2 = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "backup".to_string(),
            description: Some("Updated".to_string()),
            created_at: "2024-01-15T11:00:00Z".to_string(),
        };
        store.save_snapshot(&s2).await.unwrap();

        let loaded = store.load_snapshots(instance_id).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, s2.id);
        assert_eq!(loaded[0].description, Some("Updated".to_string()));
    }

    #[tokio::test]
    async fn get_snapshot_by_tag() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let instance_id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "test-snap".to_string(),
            description: None,
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        let found = store.get_snapshot(instance_id, "test-snap").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().tag, "test-snap");

        let not_found = store.get_snapshot(instance_id, "nonexistent").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn delete_snapshot_removes_row() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let instance_id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "to-delete".to_string(),
            description: None,
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        store.delete_snapshot(instance_id, "to-delete").await.unwrap();

        let found = store.get_snapshot(instance_id, "to-delete").await.unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn delete_nonexistent_snapshot_is_not_an_error() {
        let store = Store::open_in_memory().await.unwrap();
        store
            .delete_snapshot(InstanceId::new(), "nonexistent")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn snapshots_cascade_delete_with_instance() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let instance_id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id,
            tag: "cascade-test".to_string(),
            description: None,
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        // Delete instance — snapshots should be cascade-deleted
        store.delete_instance(instance_id).await.unwrap();

        let loaded = store.load_snapshots(instance_id).await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn different_instances_can_have_same_tag() {
        let store = Store::open_in_memory().await.unwrap();
        let cfg1 = sample_config();
        let cfg2 = sample_config();
        let id1 = cfg1.id;
        let id2 = cfg2.id;
        store
            .save_instance(&cfg1, &InstanceState::Created)
            .await
            .unwrap();
        store
            .save_instance(&cfg2, &InstanceState::Created)
            .await
            .unwrap();

        let s1 = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id: id1,
            tag: "backup".to_string(),
            description: None,
            created_at: "2024-01-15T10:30:00Z".to_string(),
        };
        let s2 = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id: id2,
            tag: "backup".to_string(),
            description: None,
            created_at: "2024-01-15T11:00:00Z".to_string(),
        };
        store.save_snapshot(&s1).await.unwrap();
        store.save_snapshot(&s2).await.unwrap();

        let loaded1 = store.load_snapshots(id1).await.unwrap();
        let loaded2 = store.load_snapshots(id2).await.unwrap();
        assert_eq!(loaded1.len(), 1);
        assert_eq!(loaded2.len(), 1);
        assert_eq!(loaded1[0].tag, "backup");
        assert_eq!(loaded2[0].tag, "backup");
    }
}
