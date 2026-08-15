use super::common::{register_with_state, sample_config, TestTempDir};
use super::*;
use andler_core::{
    BackendError, BackendHandle, GuestMutator, InstanceConfig, InstanceId, InstanceState,
    MutatorError, MutatorOp,
};
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
    /// When set, `guest_mutator` returns an `FsMutator` rooted here.
    mutator_root: Option<std::path::PathBuf>,
}

impl MaintenanceBackend {
    fn new(agent_available: bool) -> Self {
        MaintenanceBackend {
            agent_available: AtomicBool::new(agent_available),
            installs: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            spawns: AtomicUsize::new(0),
            mutator_root: None,
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

    async fn guest_mutator(
        &self,
        _handle: &BackendHandle,
    ) -> Result<Box<dyn GuestMutator>, BackendError> {
        let Some(root) = &self.mutator_root else {
            return Err(BackendError::NotImplemented {
                backend: "maintenance-mock",
                operation: "guest_mutator",
            });
        };
        Ok(Box::new(FsMutator { root: root.clone() }))
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

/// Minimal std::fs `GuestMutator` for online-provision tests: guest paths
/// map under a test root, so `apply` is observable without a guest.
struct FsMutator {
    root: std::path::PathBuf,
}

impl FsMutator {
    fn resolve(&self, path: &str) -> std::path::PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

#[async_trait::async_trait]
impl GuestMutator for FsMutator {
    fn name(&self) -> &'static str {
        "fs-mock"
    }

    async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError> {
        for op in ops {
            match op {
                MutatorOp::MkdirP { path } => {
                    std::fs::create_dir_all(self.resolve(path))
                        .map_err(|e| MutatorError::Io(format!("mkdir {path}: {e}")))?;
                }
                MutatorOp::WriteFile { path, content } => {
                    let target = self.resolve(path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| MutatorError::Io(format!("mkdir {parent:?}: {e}")))?;
                    }
                    std::fs::write(&target, content)
                        .map_err(|e| MutatorError::Io(format!("write {path}: {e}")))?;
                }
                MutatorOp::UploadFile { path, host_path } => {
                    let target = self.resolve(path);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| MutatorError::Io(format!("mkdir {parent:?}: {e}")))?;
                    }
                    std::fs::copy(host_path, &target).map_err(|e| {
                        MutatorError::Io(format!("upload {host_path:?} -> {path}: {e}"))
                    })?;
                }
                MutatorOp::Chmod { path, mode } => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        self.resolve(path),
                        std::fs::Permissions::from_mode(*mode),
                    )
                    .map_err(|e| MutatorError::Io(format!("chmod {path}: {e}")))?;
                }
                MutatorOp::Symlink { target, link } => {
                    std::os::unix::fs::symlink(target, self.resolve(link))
                        .map_err(|e| MutatorError::Io(format!("symlink {link}: {e}")))?;
                }
                other => {
                    return Err(MutatorError::Io(format!(
                        "op not supported by test mutator: {other:?}"
                    )))
                }
            }
        }
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, MutatorError> {
        std::fs::read(self.resolve(path)).map_err(|e| MutatorError::Io(format!("read {path}: {e}")))
    }

    async fn exists(&self, path: &str) -> Result<bool, MutatorError> {
        Ok(self.resolve(path).exists())
    }
}

#[tokio::test]
async fn running_provision_applies_ops_through_online_mutator() {
    let dir = TestTempDir::new();
    let guest_root = dir.path().join("guest");
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    let mut mock = MaintenanceBackend::new(true);
    mock.mutator_root = Some(guest_root.clone());
    let mock = Arc::new(mock);
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

    let src = dir.path().join("payload.bin");
    std::fs::write(&src, b"payload").unwrap();
    let ops = vec![
        MutatorOp::MkdirP {
            path: "/usr/local/bin".to_string(),
        },
        MutatorOp::UploadFile {
            path: "/usr/local/bin/agent".to_string(),
            host_path: src.clone(),
        },
        MutatorOp::Chmod {
            path: "/usr/local/bin/agent".to_string(),
            mode: 0o755,
        },
        MutatorOp::WriteFile {
            path: "/etc/agent.conf".to_string(),
            content: b"enabled = true\n".to_vec(),
        },
        MutatorOp::MkdirP {
            path: "/usr/bin".to_string(),
        },
        MutatorOp::Symlink {
            target: "/usr/local/bin/agent".to_string(),
            link: "/usr/bin/agent".to_string(),
        },
    ];

    daemon.guest_provision(id, ops).await.unwrap();

    assert_eq!(
        std::fs::read(guest_root.join("usr/local/bin/agent")).unwrap(),
        b"payload"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(guest_root.join("usr/local/bin/agent"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        std::fs::read(guest_root.join("etc/agent.conf")).unwrap(),
        b"enabled = true\n"
    );
    assert!(guest_root.join("usr/bin/agent").is_symlink());
}

#[tokio::test]
async fn provision_with_no_ops_is_refused() {
    let dir = TestTempDir::new();
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;

    let err = daemon.guest_provision(id, vec![]).await.unwrap_err();
    assert!(matches!(err, DaemonError::InvalidConfig(_)), "err: {err}");
}
