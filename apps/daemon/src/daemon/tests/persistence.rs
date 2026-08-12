use super::common::*;
use super::*;
use andler_core::{InstanceConfig, InstanceEvent, InstanceState, RenderBackend};

/// Writes an instance.toml into `<dir>/<id>/` the way the daemon would, and
/// returns the instance id.
async fn seed_toml(dir: &std::path::Path) -> InstanceId {
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.join("VARS.fd");
    let instance_dir = dir.join(cfg.id.to_string());
    tokio::fs::create_dir_all(&instance_dir).await.unwrap();
    super::types::write_instance_toml(&instance_dir, &cfg).await;
    cfg.id
}

#[tokio::test]
async fn create_instance_writes_instance_toml_on_first_transition() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;

    daemon.create_instance(cfg.clone()).await.unwrap();
    daemon
        .apply_event_and_persist(id, InstanceEvent::Start)
        .await;

    let toml_path = dir.path().join("instance.toml");
    let content = tokio::fs::read_to_string(&toml_path).await.unwrap();
    let parsed = andler_core::config::parse_instance_config_toml(&content).unwrap();
    assert_eq!(parsed, cfg);
}

#[tokio::test]
async fn daemon_without_store_does_not_panic_on_state_transitions() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.gpu.render_backend = RenderBackend::Passthrough {
        gpu_pci_id: "0000:01:00.0".to_string(),
    };
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();
    daemon.start_instance(id).await.unwrap_err();

    let status = daemon.status(id).await.unwrap();
    assert!(matches!(status.state, InstanceState::Error { .. }));
}

#[tokio::test]
async fn restore_scans_instance_tomls_from_disk() {
    let root = TestTempDir::new();
    let id = seed_toml(root.path()).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Stopped);
}

#[tokio::test]
async fn restore_ignores_non_instance_directories() {
    let root = TestTempDir::new();
    tokio::fs::create_dir_all(root.path().join("not-an-instance"))
        .await
        .unwrap();
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    assert!(daemon.list_instances().await.is_empty());
}

#[tokio::test]
async fn restore_marks_directory_without_toml_as_broken() {
    let root = TestTempDir::new();
    let id = InstanceId::new();
    tokio::fs::create_dir_all(root.path().join(id.to_string()))
        .await
        .unwrap();
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    let summaries = daemon.list_instances().await;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, id);
    assert!(summaries[0].broken_reason.is_some());
    assert!(
        summaries[0]
            .broken_reason
            .as_deref()
            .unwrap()
            .contains("missing"),
        "reason should mention the missing file: {:?}",
        summaries[0].broken_reason
    );
}

#[tokio::test]
async fn restore_marks_directory_with_invalid_toml_as_broken() {
    let root = TestTempDir::new();
    let id = InstanceId::new();
    let dir = root.path().join(id.to_string());
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("instance.toml"), b"not [valid toml")
        .await
        .unwrap();
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    let summaries = daemon.list_instances().await;
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].broken_reason.is_some());
}

#[tokio::test]
async fn restore_marks_toml_with_foreign_id_as_broken() {
    let root = TestTempDir::new();
    let dir_name_id = InstanceId::new();
    let dir = root.path().join(dir_name_id.to_string());
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let mut cfg = sample_config();
    cfg.id = InstanceId::new(); // differs from the directory name
    super::types::write_instance_toml(&dir, &cfg).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    let summaries = daemon.list_instances().await;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, dir_name_id);
    assert!(summaries[0].broken_reason.is_some());
}

/// Opens a store over a legacy (pre-phase-1) database containing `cfgs`,
/// the way production databases looked before this phase.
async fn seed_legacy_store(
    db_path: &std::path::Path,
    cfgs: &[InstanceConfig],
) -> andler_store::Store {
    {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE instances (
                id          TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                state_json  TEXT NOT NULL
            );",
        )
        .unwrap();
        for cfg in cfgs {
            conn.execute(
                "INSERT INTO instances (id, config_json, state_json) VALUES (?1, ?2, ?3)",
                (
                    cfg.id.to_string(),
                    serde_json::to_string(cfg).unwrap(),
                    serde_json::to_string(&InstanceState::Running).unwrap(),
                ),
            )
            .unwrap();
        }
    }
    andler_store::Store::open(db_path).await.unwrap()
}

#[tokio::test]
async fn restore_migrates_legacy_store_configs_to_toml() {
    let root = TestTempDir::new();
    let cfg = sample_config();
    let id = cfg.id;
    let store = seed_legacy_store(&root.path().join("andlerd.db"), &[cfg.clone()]).await;

    let daemon = Daemon::restore_with_root(store.clone(), root.path().to_path_buf())
        .await
        .unwrap();

    // Config migrated to a toml file, instance visible and stopped.
    let toml_path = root.path().join(id.to_string()).join("instance.toml");
    let content = tokio::fs::read_to_string(&toml_path).await.unwrap();
    assert_eq!(
        andler_core::config::parse_instance_config_toml(&content).unwrap(),
        cfg
    );
    assert_eq!(
        daemon.status(id).await.unwrap().state,
        InstanceState::Stopped
    );

    // Database finalized: instances table gone, schema current.
    assert_eq!(store.schema_version().await.unwrap(), 3);
    assert!(store.load_legacy_instances().await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_refuses_migration_when_toml_differs_from_store() {
    let root = TestTempDir::new();
    let cfg = sample_config();
    let id = cfg.id;

    let dir = root.path().join(id.to_string());
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let mut file_cfg = cfg.clone();
    file_cfg.name = "manually-edited".to_string();
    super::types::write_instance_toml(&dir, &file_cfg).await;

    let store = seed_legacy_store(&root.path().join("andlerd.db"), &[cfg]).await;

    let err = match Daemon::restore_with_root(store, root.path().to_path_buf()).await {
        Ok(_) => panic!("expected ConfigMigrationConflict"),
        Err(err) => err,
    };
    assert!(matches!(err, DaemonError::ConfigMigrationConflict { .. }));
}

#[tokio::test]
async fn restore_on_empty_root_yields_daemon_with_no_instances() {
    let root = TestTempDir::new();
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    let err = daemon.status(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn restored_daemon_continues_to_write_toml_on_transitions() {
    let root = TestTempDir::new();
    let id = seed_toml(root.path()).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    // A FSM transition on a restored instance rewrites instance.toml (still
    // parseable, still the same instance).
    daemon
        .apply_event_and_persist(id, InstanceEvent::Start)
        .await;

    let toml_path = root.path().join(id.to_string()).join("instance.toml");
    let content = tokio::fs::read_to_string(&toml_path).await.unwrap();
    let parsed = andler_core::config::parse_instance_config_toml(&content).unwrap();
    assert_eq!(parsed.id, id);
}
