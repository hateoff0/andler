use super::common::*;
use super::*;
use andler_core::BackendHandle;

#[tokio::test]
async fn health_check_ignores_non_running_instances() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    daemon.run_health_check_once().await;

    let instances = daemon.instances.read().await;
    let record = instances
        .get(&id)
        .expect("instance must still be registered");
    assert_eq!(record.state, InstanceState::Created);
}

#[tokio::test]
async fn health_check_skips_running_instance_without_handle() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Running,
            handle: None,
        },
    );

    daemon.run_health_check_once().await;

    let instances = daemon.instances.read().await;
    let record = instances
        .get(&id)
        .expect("instance must still be registered");
    assert_eq!(record.state, InstanceState::Running);
}

#[tokio::test]
async fn mark_instance_crashed_transitions_to_error_and_clears_handle() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Running,
            handle: Some(BackendHandle("qemu:test".to_string())),
        },
    );

    daemon
        .mark_instance_crashed(id, "process exited unexpectedly".to_string())
        .await
        .unwrap();

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert!(matches!(record.state, InstanceState::Error { .. }));
    assert!(record.handle.is_none());
}

#[tokio::test]
async fn mark_instance_crashed_on_unknown_id_returns_instance_not_found() {
    let daemon = Daemon::new();

    let err = daemon
        .mark_instance_crashed(InstanceId::new(), "boom".to_string())
        .await
        .unwrap_err();

    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn mark_instance_stopped_cleanly_transitions_to_stopped_and_clears_handle() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Running,
            handle: Some(BackendHandle("qemu:test".to_string())),
        },
    );

    daemon.mark_instance_stopped_cleanly(id).await.unwrap();

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert_eq!(record.state, InstanceState::Stopped);
    assert!(record.handle.is_none());
}

#[tokio::test]
async fn mark_instance_stopped_cleanly_on_unknown_id_returns_instance_not_found() {
    let daemon = Daemon::new();

    let err = daemon
        .mark_instance_stopped_cleanly(InstanceId::new())
        .await
        .unwrap_err();

    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}
