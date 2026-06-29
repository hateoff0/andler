use super::common::*;
use super::*;
use andler_core::RenderBackend;

// --- list_instances ------------------------------------------------------

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
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let cfg = sample_config();
    let id = cfg.id;
    store
        .save_instance(&cfg, &InstanceState::Created)
        .await
        .unwrap();

    let daemon = Daemon::restore(store).await.unwrap();
    let summaries = daemon.list_instances().await;

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, id);
    assert_eq!(summaries[0].name, cfg.name);
}

// --- get_instance_config -------------------------------------------------

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
