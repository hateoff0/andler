use std::path::Path;
use std::sync::{Arc, Mutex};

use andler_core::{InstanceConfig, InstanceId};
use rusqlite::Connection;

use crate::error::StoreError;

/// SQLite store for snapshot metadata only. Instance configuration lives in
/// `instance.toml` files (daemon registry), never here.
///
/// Schema versioning via `PRAGMA user_version`:
/// - 0: legacy database, `instances` table may exist; the daemon migrates
///   configs out of it (`load_legacy_instances`) then calls
///   `finalize_config_migration` (drops `instances`, version -> 2).
/// - 1: `instances` table still present, waiting for config migration.
/// - 2: current: `snapshots` only.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
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

    /// Current `PRAGMA user_version`.
    pub async fn schema_version(&self) -> Result<i64, StoreError> {
        self.run_blocking(|conn| {
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            Ok(version)
        })
        .await
    }

    /// Loads instance configs from the legacy `instances` table (pre-phase-1
    /// databases). Returns an error on any corrupt row so the daemon can fail
    /// the migration explicitly instead of guessing.
    pub async fn load_legacy_instances(&self) -> Result<Vec<InstanceConfig>, StoreError> {
        self.run_blocking(|conn| {
            let has_table: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'instances')",
                [],
                |r| r.get(0),
            )?;
            if !has_table {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare("SELECT config_json FROM instances")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut result = Vec::new();
            for row in rows {
                let config_json = row?;
                let config: InstanceConfig = serde_json::from_str(&config_json)?;
                result.push(config);
            }
            Ok(result)
        })
        .await
    }

    /// Drops the legacy `instances` table and marks the schema current.
    /// Called once all legacy configs have been written as `instance.toml`
    /// files. The `snapshots` FK referencing `instances` is dropped together
    /// with the table (SQLite removes foreign keys of dropped tables; the
    /// store never enables `foreign_keys`).
    pub async fn finalize_config_migration(&self) -> Result<(), StoreError> {
        self.run_blocking(|conn| {
            conn.execute_batch("DROP TABLE IF EXISTS instances; PRAGMA user_version = 2;")?;
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
    // Some distro sqlite builds default `foreign_keys` to ON, which would
    // cascade-delete snapshot rows when the legacy `instances` table is
    // dropped. The store never relies on FK enforcement, so pin it off.
    conn.pragma_update(None, "foreign_keys", false)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 2 {
        return Ok(());
    }

    let has_legacy_instances: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'instances')",
        [],
        |r| r.get(0),
    )?;

    // Legacy databases already carry a snapshots table (with an FK to
    // instances); CREATE IF NOT EXISTS leaves it untouched, and dropping
    // `instances` later removes the FK.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS snapshots (
            id          TEXT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            tag         TEXT NOT NULL,
            description TEXT,
            created_at  TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_snapshots_instance_tag
            ON snapshots(instance_id, tag);",
    )?;

    let target = if has_legacy_instances { 1 } else { 2 };
    conn.execute_batch(&format!("PRAGMA user_version = {target};"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceKind, InstanceState, MemoryConfig, NetworkConfig,
    };
    use std::path::{Path, PathBuf};

    /// Simple per-test temp dir, mirroring the daemon tests' helper (no
    /// tempfile dependency in the workspace).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "andler-store-test-{}-{}",
                std::process::id(),
                InstanceId::new().to_string()
            ));
            std::fs::create_dir_all(&path).expect("create test temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: andler_core::CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            schema_version: andler_core::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    /// Builds a legacy (pre-phase-1) database file: `instances` +
    /// `snapshots` tables with one row each.
    fn build_legacy_db(path: &Path) -> InstanceConfig {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE instances (
                id          TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                state_json  TEXT NOT NULL
            );
            CREATE TABLE snapshots (
                id          TEXT PRIMARY KEY,
                instance_id TEXT NOT NULL,
                tag         TEXT NOT NULL,
                description TEXT,
                created_at  TEXT NOT NULL,
                FOREIGN KEY (instance_id) REFERENCES instances(id) ON DELETE CASCADE
            );
            CREATE UNIQUE INDEX idx_snapshots_instance_tag ON snapshots(instance_id, tag);",
        )
        .unwrap();

        let cfg = sample_config();
        conn.execute(
            "INSERT INTO instances (id, config_json, state_json) VALUES (?1, ?2, ?3)",
            (
                cfg.id.to_string(),
                serde_json::to_string(&cfg).unwrap(),
                serde_json::to_string(&InstanceState::Running).unwrap(),
            ),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO snapshots (id, instance_id, tag, description, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                uuid::Uuid::new_v4().to_string(),
                cfg.id.to_string(),
                "legacy-snap".to_string(),
                "taken before migration".to_string(),
                "2026-01-01T00:00:00+00:00".to_string(),
            ),
        )
        .unwrap();
        cfg
    }

    #[tokio::test]
    async fn fresh_store_starts_at_current_schema() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn legacy_store_migrates_configs_then_finalizes() {
        let dir = TempDir::new();
        let db_path = dir.path().join("andlerd.db");
        let expected = build_legacy_db(&db_path);

        let store = Store::open(&db_path).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 1);

        let legacy = store.load_legacy_instances().await.unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0], expected);
        assert_eq!(legacy[0].name, "test-vm");

        // Snapshots are readable before and after the migration.
        let before = store.load_snapshots(expected.id).await.unwrap();
        assert_eq!(before.len(), 1);

        store.finalize_config_migration().await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 2);

        let legacy = store.load_legacy_instances().await.unwrap();
        assert!(legacy.is_empty(), "instances table must be dropped");

        let snapshots = store.load_snapshots(expected.id).await.unwrap();
        assert_eq!(
            snapshots.len(),
            1,
            "migration must not delete snapshot metadata"
        );
        assert_eq!(snapshots[0].tag, "legacy-snap");
    }

    #[tokio::test]
    async fn corrupt_legacy_config_fails_migration() {
        let dir = TempDir::new();
        let db_path = dir.path().join("andlerd.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE instances (
                id          TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                state_json  TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO instances (id, config_json, state_json) VALUES (?1, ?2, ?3)",
            ("abc".to_string(), "not json".to_string(), "{}".to_string()),
        )
        .unwrap();
        drop(conn);

        let store = Store::open(&db_path).await.unwrap();
        let err = store.load_legacy_instances().await.unwrap_err();
        assert!(matches!(err, StoreError::Serde(_)));
    }

    #[tokio::test]
    async fn snapshots_round_trip_without_instances_table() {
        let store = Store::open_in_memory().await.unwrap();
        let id = InstanceId::new();
        let snapshot = StoredSnapshot {
            id: uuid::Uuid::new_v4(),
            instance_id: id,
            tag: "snap-first".to_string(),
            description: Some("first".to_string()),
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        store.save_snapshot(&snapshot).await.unwrap();

        let loaded = store.load_snapshots(id).await.unwrap();
        assert_eq!(loaded, vec![snapshot.clone()]);

        let by_tag = store.get_snapshot(id, "snap-first").await.unwrap();
        assert_eq!(by_tag, Some(snapshot.clone()));

        store.delete_snapshot(id, "snap-first").await.unwrap();
        assert!(store
            .get_snapshot(id, "snap-first")
            .await
            .unwrap()
            .is_none());
        assert!(store.load_snapshots(id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn save_snapshot_with_existing_tag_replaces() {
        let store = Store::open_in_memory().await.unwrap();
        let id = InstanceId::new();
        store
            .save_snapshot(&StoredSnapshot {
                id: uuid::Uuid::new_v4(),
                instance_id: id,
                tag: "same-tag".to_string(),
                description: Some("first".to_string()),
                created_at: "2026-01-01T00:00:00+00:00".to_string(),
            })
            .await
            .unwrap();
        store
            .save_snapshot(&StoredSnapshot {
                id: uuid::Uuid::new_v4(),
                instance_id: id,
                tag: "same-tag".to_string(),
                description: Some("second".to_string()),
                created_at: "2026-01-02T00:00:00+00:00".to_string(),
            })
            .await
            .unwrap();

        let snapshots = store.load_snapshots(id).await.unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].description.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn reopens_of_current_store_are_idempotent() {
        let dir = TempDir::new();
        let db_path = dir.path().join("andlerd.db");
        let id = InstanceId::new();

        {
            let store = Store::open(&db_path).await.unwrap();
            store
                .save_snapshot(&StoredSnapshot {
                    id: uuid::Uuid::new_v4(),
                    instance_id: id,
                    tag: "s".to_string(),
                    description: None,
                    created_at: "2026-01-01T00:00:00+00:00".to_string(),
                })
                .await
                .unwrap();
        }

        let reopened = Store::open(&db_path).await.unwrap();
        assert_eq!(reopened.schema_version().await.unwrap(), 2);
        assert_eq!(reopened.load_snapshots(id).await.unwrap().len(), 1);
    }
}
