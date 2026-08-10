use super::common::*;
use super::*;
use andler_core::RenderBackend;

#[tokio::test]
async fn list_instances_on_empty_daemon_returns_empty_vec() {
    let daemon = Daemon::new();
    assert!(daemon.list_instances().await.is_empty());
}

#[tokio::test]
async fn list_instances_returns_one_summary_per_created_instance() {
    let daemon = Daemon::new();
    let cfg1 = sample_config();
    let mut cfg2 = sample_config();
    cfg2.name = "second-vm".to_string();

    let id1 = daemon.create_instance(cfg1).await.unwrap();
    let id2 = daemon.create_instance(cfg2).await.unwrap();

    let mut summaries = daemon.list_instances().await;
    summaries.sort_by_key(|s| s.name.clone());

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, id2);
    assert_eq!(summaries[0].name, "second-vm");
    assert_eq!(summaries[0].state, InstanceState::Created);
    assert_eq!(summaries[1].id, id1);
    assert_eq!(summaries[1].state, InstanceState::Created);
}

#[tokio::test]
async fn list_instances_reflects_state_after_failed_start() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.gpu.render_backend = RenderBackend::Passthrough {
        gpu_pci_id: "0000:01:00.0".to_string(),
    };
    let id = daemon.create_instance(cfg).await.unwrap();
    daemon.start_instance(id).await.unwrap_err();

    let summaries = daemon.list_instances().await;
    assert_eq!(summaries.len(), 1);
    assert!(matches!(summaries[0].state, InstanceState::Error { .. }));
}

#[tokio::test]
async fn list_instances_after_restore_includes_restored_instances() {
    let root = TestTempDir::new();
    let mut cfg = sample_config();
    cfg.disk.path = root.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = root.path().join("VARS.fd");
    let id = cfg.id;
    let instance_dir = root.path().join(id.to_string());
    tokio::fs::create_dir_all(&instance_dir).await.unwrap();
    super::types::write_instance_toml(&instance_dir, &cfg).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();
    let summaries = daemon.list_instances().await;

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, id);
    assert_eq!(summaries[0].name, cfg.name);
}

#[tokio::test]
async fn get_instance_config_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .get_instance_config(InstanceId::new())
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn get_instance_config_returns_full_config_unchanged() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = daemon.create_instance(cfg.clone()).await.unwrap();

    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched, cfg);
}

#[tokio::test]
async fn get_instance_config_reflects_current_record_not_a_stale_snapshot() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg.clone()).await.unwrap();

    cfg.name = "renamed-vm".to_string();
    daemon.create_instance(cfg.clone()).await.unwrap();

    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched.name, "renamed-vm");
}

#[tokio::test]
async fn update_instance_config_succeeds_on_idle_instance() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = daemon.create_instance(cfg.clone()).await.unwrap();

    let mut new_cfg = cfg;
    new_cfg.name = "updated-name".to_string();

    daemon.update_instance_config(id, new_cfg).await.unwrap();

    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched.name, "updated-name");
}

#[tokio::test]
async fn config_status_shows_pending_file_edit_on_running_instance() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;
    daemon.create_instance(cfg.clone()).await.unwrap();
    register_with_state(&daemon, cfg.clone(), InstanceState::Running, None).await;

    let mut edited = cfg.clone();
    edited.name = "pending-name".to_string();
    tokio::fs::write(
        dir.path().join("instance.toml"),
        andler_core::instance_config_to_toml(&edited).unwrap(),
    )
    .await
    .unwrap();

    let (_, diffs, file_error, _) = daemon.config_status(id).await.unwrap();
    assert!(file_error.is_none());
    let name_diff = diffs
        .iter()
        .find(|(key, _, _)| key == "name")
        .expect("pending file edit must be reported as a diff");
    assert_eq!(name_diff.1, "pending-name");
    assert_eq!(name_diff.2, cfg.name);

    // The live config is untouched by the file edit.
    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched.name, cfg.name);
}

#[tokio::test]
async fn config_status_on_idle_instance_applies_file_edit() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;
    daemon.create_instance(cfg.clone()).await.unwrap();

    let mut edited = cfg.clone();
    edited.name = "applied-name".to_string();
    tokio::fs::write(
        dir.path().join("instance.toml"),
        andler_core::instance_config_to_toml(&edited).unwrap(),
    )
    .await
    .unwrap();

    let (_, diffs, file_error, _) = daemon.config_status(id).await.unwrap();
    assert!(file_error.is_none());
    assert!(
        diffs.is_empty(),
        "idle instance: file edit is applied, not pending"
    );

    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched.name, "applied-name");
}

#[tokio::test]
async fn config_status_reports_invalid_file_without_touching_memory() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;
    daemon.create_instance(cfg.clone()).await.unwrap();
    register_with_state(&daemon, cfg.clone(), InstanceState::Running, None).await;

    tokio::fs::write(dir.path().join("instance.toml"), b"not [valid toml")
        .await
        .unwrap();

    let (_, diffs, file_error, _) = daemon.config_status(id).await.unwrap();
    assert!(diffs.is_empty());
    assert!(file_error.is_some(), "invalid toml must be reported");

    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_eq!(fetched.name, cfg.name);
}

#[tokio::test]
async fn update_instance_config_rejected_while_running() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = daemon.create_instance(cfg.clone()).await.unwrap();
    daemon.start_instance(id).await.unwrap();

    let mut new_cfg = cfg;
    new_cfg.name = "should-not-apply".to_string();

    let err = daemon
        .update_instance_config(id, new_cfg)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceMustBeStopped(_, _)));

    // config on record must be untouched
    let fetched = daemon.get_instance_config(id).await.unwrap();
    assert_ne!(fetched.name, "should-not-apply");
}
