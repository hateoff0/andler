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

    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Created);
}

#[tokio::test]
async fn health_check_skips_running_instance_without_handle() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Running, None).await;

    daemon.run_health_check_once().await;

    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Running);
}

#[tokio::test]
async fn mark_instance_crashed_transitions_to_error_and_clears_handle() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    register_with_state(
        &daemon,
        cfg,
        InstanceState::Running,
        Some(BackendHandle("qemu:test".to_string())),
    )
    .await;

    daemon
        .mark_instance_crashed(id, "process exited unexpectedly".to_string())
        .await
        .unwrap();

    let handle = daemon.handle_for(id).await.unwrap();
    assert!(matches!(handle.state(), InstanceState::Error { .. }));
    assert!(handle.backend_handle().is_none());
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
    register_with_state(
        &daemon,
        cfg,
        InstanceState::Running,
        Some(BackendHandle("qemu:test".to_string())),
    )
    .await;

    daemon.mark_instance_stopped_cleanly(id).await.unwrap();

    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Stopped);
    assert!(handle.backend_handle().is_none());
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
