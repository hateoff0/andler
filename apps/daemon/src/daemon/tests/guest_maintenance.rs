use super::common::{register_with_state, sample_config, TestTempDir};
use super::*;
use andler_core::{BackendError, BackendHandle, InstanceConfig, InstanceId, InstanceState};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;

/// Backend whose guest agent appears immediately after spawn; records
/// installs/removes and whether a stop happened.
struct MaintenanceBackend {
    agent_available: AtomicBool,
    installs: AtomicUsize,
    removes: AtomicUsize,
    stops: AtomicUsize,
    spawns: AtomicUsize,
}

impl MaintenanceBackend {
    fn new(agent_available: bool) -> Self {
        MaintenanceBackend {
            agent_available: AtomicBool::new(agent_available),
            installs: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            spawns: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl andler_core::HypervisorBackend for MaintenanceBackend {
    fn name(&self) -> &'static str {
        "maintenance-mock"
    }

    fn supported_render_backends(&self) -> &[andler_core::RenderBackend] {
        &[]
    }

    async fn spawn(&self, _cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        self.spawns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(BackendHandle("maintenance-mock:vm".to_string()))
    }

    async fn pause(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
        Ok(())
    }

    async fn resume(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
        Ok(())
    }

    async fn stop(&self, _handle: &BackendHandle, _graceful: bool) -> Result<(), BackendError> {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn status(
        &self,
        _handle: &BackendHandle,
    ) -> Result<andler_core::BackendStatus, BackendError> {
        Ok(andler_core::BackendStatus {
            state: InstanceState::Running,
            detail: None,
            clean_shutdown: false,
        })
    }

    async fn is_guest_agent_available(
        &self,
        _handle: &BackendHandle,
    ) -> Result<bool, BackendError> {
        Ok(self
            .agent_available
            .load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn guest_exec_install(
        &self,
        _handle: &BackendHandle,
        _package: &str,
    ) -> Result<(), BackendError> {
        self.installs
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn guest_exec_remove(
        &self,
        _handle: &BackendHandle,
        _package: &str,
    ) -> Result<(), BackendError> {
        self.removes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn guest_exec_command(
        &self,
        _handle: &BackendHandle,
        _argv: &[String],
        _timeout: Option<std::time::Duration>,
    ) -> Result<andler_core::GuestExecOutput, BackendError> {
        Err(BackendError::NotImplemented {
            backend: "maintenance-mock",
            operation: "guest_exec_command",
        })
    }

    fn metrics_stream(
        &self,
        _handle: &BackendHandle,
    ) -> futures_core::stream::BoxStream<'_, andler_core::ResourceMetrics> {
        Box::pin(futures_util::stream::empty())
    }

    fn log_stream(
        &self,
        _handle: &BackendHandle,
    ) -> futures_core::stream::BoxStream<'_, andler_core::LogLine> {
        Box::pin(futures_util::stream::empty())
    }
}

async fn stopped_instance_with_maintenance_backend(
    dir: &std::path::Path,
    agent_available: bool,
) -> (Daemon, Arc<MaintenanceBackend>, InstanceId) {
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"not a real qcow2").unwrap();
    let id = cfg.id;
    let mock = Arc::new(MaintenanceBackend::new(agent_available));
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;
    daemon
        .backends
        .insert(andler_core::BackendKind::Qemu, mock.clone());
    (daemon, mock, id)
}

#[tokio::test]
async fn stopped_install_auto_starts_installs_and_stops_again() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = stopped_instance_with_maintenance_backend(dir.path(), true).await;

    daemon
        .install_guest_agent(id, "htop".to_string(), false)
        .await
        .unwrap();

    assert_eq!(mock.spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(mock.installs.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(mock.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Stopped);
    assert!(handle.backend_handle().is_none());
}

#[tokio::test]
async fn stopped_remove_auto_starts_removes_and_stops_again() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = stopped_instance_with_maintenance_backend(dir.path(), true).await;

    daemon
        .remove_guest_agent(id, "htop".to_string(), false)
        .await
        .unwrap();

    assert_eq!(mock.removes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(mock.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Stopped);
}

#[tokio::test]
async fn stopped_install_without_agent_fails_and_leaves_instance_stopped() {
    let dir = TestTempDir::new();
    std::env::set_var("ANDLERD_GUEST_AGENT_WAIT_SECS", "5");
    let (daemon, mock, id) = stopped_instance_with_maintenance_backend(dir.path(), false).await;

    let err = daemon
        .install_guest_agent(id, "htop".to_string(), false)
        .await
        .unwrap_err();

    std::env::remove_var("ANDLERD_GUEST_AGENT_WAIT_SECS");
    assert!(
        matches!(&err, DaemonError::GuestAgentUnavailable { message, .. } if message.contains("--offline")),
        "err: {err}"
    );
    assert_eq!(mock.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(handle.state(), InstanceState::Stopped);
    assert!(handle.backend_handle().is_none());
}

#[tokio::test]
async fn offline_flag_on_running_instance_is_refused() {
    let dir = TestTempDir::new();
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    let mock = Arc::new(MaintenanceBackend::new(true));
    register_with_state(
        &daemon,
        cfg,
        InstanceState::Running,
        Some(BackendHandle("maintenance-mock:vm".to_string())),
    )
    .await;
    daemon
        .backends
        .insert(andler_core::BackendKind::Qemu, mock.clone());

    let err = daemon
        .install_guest_agent(id, "htop".to_string(), true)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InvalidConfig(_)), "err: {err}");
}
