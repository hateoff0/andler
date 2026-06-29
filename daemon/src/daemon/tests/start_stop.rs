use super::common::*;
use super::*;
use andler_core::RenderBackend;

#[tokio::test]
async fn start_instance_rejects_passthrough_via_backend_validation() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.gpu.render_backend = RenderBackend::Passthrough {
        gpu_pci_id: "0000:01:00.0".to_string(),
    };
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon.start_instance(id).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::Backend(BackendError::InvalidConfig { .. })
    ));

    let status = daemon.status(id).await.unwrap();
    assert!(matches!(status.state, InstanceState::Error { .. }));
}

#[tokio::test]
async fn pause_before_start_returns_handle_not_found() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon.pause_instance(id).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::Backend(BackendError::HandleNotFound(_))
    ));
}

#[tokio::test]
async fn pause_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon.pause_instance(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn resume_before_start_returns_handle_not_found() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon.resume_instance(id).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::Backend(BackendError::HandleNotFound(_))
    ));
}

#[tokio::test]
async fn resume_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon.resume_instance(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn stop_before_start_returns_handle_not_found() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon.stop_instance(id, true).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::Backend(BackendError::HandleNotFound(_))
    ));
}
