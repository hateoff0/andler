use super::common::*;
use super::*;
use andler_core::{BackendError, RenderBackend};

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
async fn start_instance_no_longer_rejected_by_fsm_when_stopped_or_errored() {
    for initial_state in [
        InstanceState::Stopped,
        InstanceState::Error {
            message: "previous crash".to_string(),
        },
    ] {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        register_with_state(&daemon, cfg, initial_state.clone(), None).await;

        match daemon.start_instance(id).await {
            Ok(()) => {
                let _ = daemon.stop_instance(id, false).await;
            }
            Err(DaemonError::InvalidTransition(_)) => panic!(
                "expected {initial_state:?} to accept Start (restart), \
                 but the FSM still rejected it"
            ),
            Err(_other_backend_error) => {} // fine — failed for an unrelated reason
        }
    }
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
async fn start_instance_with_missing_disk_file_fails_immediately_not_after_health_check() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = std::path::PathBuf::from("/tmp/andler-test-definitely-does-not-exist.qcow2");
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon.start_instance(id).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::Backend(BackendError::Io(ref msg)) if msg.contains("disk file not found")
    ));

    let status = daemon.status(id).await.unwrap();
    assert!(matches!(status.state, InstanceState::Error { .. }));
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

#[tokio::test]
async fn stop_after_crash_returns_already_stopped_not_raw_handle_error() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    daemon
        .mark_instance_crashed(id, "process is not running".to_string())
        .await
        .unwrap();

    let err = daemon.stop_instance(id, true).await.unwrap_err();
    assert!(
        matches!(err, DaemonError::InstanceAlreadyStopped(_, ref reason) if reason == "process is not running")
    );
}

#[tokio::test]
async fn apply_event_and_persist_pause_transitions_running_to_paused() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();
    let handle = daemon.handle_for(id).await.unwrap();
    handle.transition(InstanceEvent::Start).await.unwrap();
    handle
        .transition(InstanceEvent::StartCompleted)
        .await
        .unwrap();

    daemon
        .apply_event_and_persist(id, andler_core::InstanceEvent::Pause)
        .await;

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Paused);
}

#[tokio::test]
async fn apply_event_and_persist_resume_transitions_paused_to_running() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();
    let handle = daemon.handle_for(id).await.unwrap();
    handle.transition(InstanceEvent::Start).await.unwrap();
    handle
        .transition(InstanceEvent::StartCompleted)
        .await
        .unwrap();
    handle.transition(InstanceEvent::Pause).await.unwrap();

    daemon
        .apply_event_and_persist(id, andler_core::InstanceEvent::Resume)
        .await;

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Running);
}

#[tokio::test]
async fn apply_event_and_persist_invalid_transition_does_not_panic_or_change_state() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();
    // Fresh instance is Created; Pause is not a valid transition from Created.

    daemon
        .apply_event_and_persist(id, andler_core::InstanceEvent::Pause)
        .await;

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Created);
}
