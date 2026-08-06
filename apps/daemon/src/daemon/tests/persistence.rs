use super::common::*;
use super::*;
use andler_core::RenderBackend;

#[tokio::test]
async fn with_store_persists_created_instance() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::with_store(store.clone());
    let cfg = sample_config();
    let id = cfg.id;

    daemon.create_instance(cfg.clone()).await.unwrap();

    let stored = store.load_instance(id).await.unwrap();
    assert_eq!(stored.config, cfg);
    assert_eq!(stored.state, InstanceState::Created);
}

#[tokio::test]
async fn with_store_persists_failed_start_as_error_state() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::with_store(store.clone());
    let mut cfg = sample_config();
    cfg.gpu.render_backend = RenderBackend::Passthrough {
        gpu_pci_id: "0000:01:00.0".to_string(),
    };
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    daemon.start_instance(id).await.unwrap_err();

    let stored = store.load_instance(id).await.unwrap();
    assert!(matches!(stored.state, InstanceState::Error { .. }));
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
async fn restore_recreates_daemon_from_store_contents() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let cfg = sample_config();
    let id = cfg.id;
    store
        .save_instance(&cfg, &InstanceState::Created)
        .await
        .unwrap();

    let daemon = Daemon::restore(store).await.unwrap();

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Created);
}

#[tokio::test]
async fn restore_marks_running_instance_as_error_since_handle_is_lost() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let cfg = sample_config();
    let id = cfg.id;
    store
        .save_instance(&cfg, &InstanceState::Running)
        .await
        .unwrap();

    let daemon = Daemon::restore(store).await.unwrap();

    let status = daemon.status(id).await.unwrap();
    assert!(
        matches!(status.state, InstanceState::Error { .. }),
        "expected Running to be restored as Error, got {:?}",
        status.state
    );
}

#[tokio::test]
async fn restore_marks_every_non_terminal_state_as_error() {
    for lost_state in [
        InstanceState::Starting,
        InstanceState::Running,
        InstanceState::Paused,
        InstanceState::Stopping,
    ] {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store.save_instance(&cfg, &lost_state).await.unwrap();

        let daemon = Daemon::restore(store).await.unwrap();
        let status = daemon.status(id).await.unwrap();
        assert!(
            matches!(status.state, InstanceState::Error { .. }),
            "expected {lost_state:?} to be restored as Error, got {:?}",
            status.state
        );
    }
}

#[tokio::test]
async fn restore_keeps_terminal_states_unchanged() {
    for terminal_state in [
        InstanceState::Created,
        InstanceState::Stopped,
        InstanceState::Error {
            message: "previous failure".to_string(),
        },
    ] {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store.save_instance(&cfg, &terminal_state).await.unwrap();

        let daemon = Daemon::restore(store).await.unwrap();
        let status = daemon.status(id).await.unwrap();
        assert_eq!(
            status.state, terminal_state,
            "expected terminal state {terminal_state:?} to survive restore unchanged"
        );
    }
}

#[tokio::test]
async fn restore_on_empty_store_yields_daemon_with_no_instances() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::restore(store).await.unwrap();

    let err = daemon.status(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn restored_daemon_continues_to_persist_further_transitions() {
    let store = andler_store::Store::open_in_memory().await.unwrap();
    let daemon = Daemon::restore(store.clone()).await.unwrap();

    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg.clone()).await.unwrap();

    let stored = store.load_instance(id).await.unwrap();
    assert_eq!(stored.config, cfg);
    assert_eq!(stored.state, InstanceState::Created);
}
