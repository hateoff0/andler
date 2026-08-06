use std::path::Path;
use std::sync::{Arc, Mutex};

use andler_core::{InstanceConfig, InstanceId, InstanceState};
use rusqlite::Connection;

use crate::error::StoreError;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInstance {
    pub config: InstanceConfig,
    pub state: InstanceState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub id: uuid::Uuid,
    pub instance_id: InstanceId,
    pub tag: String,
    pub description: Option<String>,
    pub created_at: String,
}

impl Store {
    /// Runs `f` on a blocking thread with a lock on the shared connection, propagating
    /// both task-join failures and `f`'s own errors as a single `StoreError`.
    fn run_blocking<F, T>(&self, f: F) -> impl std::future::Future<Output = Result<T, StoreError>>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
                f(&guard)
            })
            .await?
        }
    }

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

    pub async fn save_instance(
        &self,
        cfg: &InstanceConfig,
        state: &InstanceState,
    ) -> Result<(), StoreError> {
        let id = cfg.id;
        let config_json = serde_json::to_string(cfg)?;
        let state_json = serde_json::to_string(state)?;

        self.run_blocking(move |conn| {
            conn.execute(
                "INSERT INTO instances (id, config_json, state_json) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET \
                     config_json = excluded.config_json, \
                     state_json = excluded.state_json",
                (id.to_string(), config_json, state_json),
            )?;
            Ok(())
        })
        .await
    }

    pub async fn save_state(
        &self,
        id: InstanceId,
        state: &InstanceState,
    ) -> Result<(), StoreError> {
        let state_json = serde_json::to_string(state)?;

        let rows_changed = self
            .run_blocking(move |conn| {
                let rows = conn.execute(
                    "UPDATE instances SET state_json = ?1 WHERE id = ?2",
                    (state_json, id.to_string()),
                )?;
                Ok(rows)
            })
            .await?;

        if rows_changed == 0 {
            return Err(StoreError::NotFound(id));
        }

        Ok(())
    }

    pub async fn load_instance(&self, id: InstanceId) -> Result<StoredInstance, StoreError> {
        self.run_blocking(move |conn| {
            let row = conn
                .query_row(
                    "SELECT config_json, state_json FROM instances WHERE id = ?1",
                    [id.to_string()],
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
        .await
    }

    pub async fn load_all(&self) -> Result<Vec<StoredInstance>, StoreError> {
        self.run_blocking(move |conn| {
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
        .await
    }

    pub async fn delete_instance(&self, id: InstanceId) -> Result<(), StoreError> {
        self.run_blocking(move |conn| {
            conn.execute("DELETE FROM instances WHERE id = ?1", [id.to_string()])?;
            Ok(())
        })
        .await
    }

    pub async fn save_snapshot(&self, snapshot: &StoredSnapshot) -> Result<(), StoreError> {
        let id = snapshot.id.to_string();
        let instance_id = snapshot.instance_id.to_string();
        let tag = snapshot.tag.clone();
        let description = snapshot.description.clone();
        let created_at = snapshot.created_at.clone();

        self.run_blocking(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO snapshots (id, instance_id, tag, description, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (id, instance_id, tag, description, created_at),
            )?;
            Ok(())
        })
        .await
    }

    pub async fn load_snapshots(
        &self,
        instance_id: InstanceId,
    ) -> Result<Vec<StoredSnapshot>, StoreError> {
        let instance_id_str = instance_id.to_string();

        self.run_blocking(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, instance_id, tag, description, created_at \
                 FROM snapshots WHERE instance_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map([instance_id_str], row_to_stored_snapshot)?;

            let mut result = Vec::new();
            for row in rows {
                result.push(row?);
            }
            Ok(result)
        })
        .await
    }

    pub async fn get_snapshot(
        &self,
        instance_id: InstanceId,
        tag: &str,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let instance_id_str = instance_id.to_string();
        let tag = tag.to_string();

        self.run_blocking(move |conn| {
            let result = conn.query_row(
                "SELECT id, instance_id, tag, description, created_at \
                 FROM snapshots WHERE instance_id = ?1 AND tag = ?2",
                (instance_id_str, tag),
                row_to_stored_snapshot,
            );

            match result {
                Ok(snapshot) => Ok(Some(snapshot)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(StoreError::Sqlite(e)),
            }
        })
        .await
    }

    pub async fn delete_snapshot(
        &self,
        instance_id: InstanceId,
        tag: &str,
    ) -> Result<(), StoreError> {
        let instance_id_str = instance_id.to_string();
        let tag = tag.to_string();

        self.run_blocking(move |conn| {
            conn.execute(
                "DELETE FROM snapshots WHERE instance_id = ?1 AND tag = ?2",
                (instance_id_str, tag),
            )?;
            Ok(())
        })
        .await
    }
}

fn row_to_stored_instance(
    (config_json, state_json): (String, String),
) -> Result<StoredInstance, StoreError> {
    let config: InstanceConfig = serde_json::from_str(&config_json)?;
    let state: InstanceState = serde_json::from_str(&state_json)?;
    Ok(StoredInstance { config, state })
}

fn row_to_stored_snapshot(row: &rusqlite::Row<'_>) -> Result<StoredSnapshot, rusqlite::Error> {
    Ok(StoredSnapshot {
        id: uuid::Uuid::parse_str(row.get::<_, String>(0)?.as_str())
            .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
        instance_id: row
            .get::<_, String>(1)?
            .parse::<InstanceId>()
            .map_err(|_| rusqlite::Error::InvalidParameterName("instance_id".into()))?,
        tag: row.get(2)?,
        description: row.get(3)?,
        created_at: row.get(4)?,
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceKind, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: andler_core::CdromBus::Ide,
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
    async fn save_instance_does_not_cascade_delete_snapshots() {
        let store = Store::open_in_memory().await.unwrap();
        let mut cfg = sample_config();
        let id = cfg.id;

        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id: id,
            tag: "snap-first".to_string(),
            description: Some("first".to_string()),
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        cfg.name = "renamed".to_string();
        store
            .save_instance(&cfg, &InstanceState::Starting)
            .await
            .unwrap();
        store
            .save_instance(&cfg, &InstanceState::Running)
            .await
            .unwrap();

        let snapshots = store.load_snapshots(id).await.unwrap();
        assert_eq!(
            snapshots.len(),
            1,
            "save_instance must not delete snapshot metadata"
        );
        assert_eq!(snapshots[0].tag, "snap-first");
        assert_eq!(snapshots[0].description.as_deref(), Some("first"));
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

        store.save_state(id, &InstanceState::Running).await.unwrap();

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
        assert_eq!(loaded[0].description, Some("Before update".to_string()));
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

        let not_found = store
            .get_snapshot(instance_id, "nonexistent")
            .await
            .unwrap();
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

        store
            .delete_snapshot(instance_id, "to-delete")
            .await
            .unwrap();

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
