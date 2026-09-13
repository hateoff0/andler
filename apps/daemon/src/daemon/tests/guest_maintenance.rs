use super::common::{register_with_state, sample_config, TestTempDir};
use super::*;
use andler_core::{
    BackendError, BackendHandle, GuestMutator, InstanceConfig, InstanceId, InstanceState,
    MutatorError, MutatorOp,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        .install_guest_agent(id, "htop".to_string(), false, None)
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
        .remove_guest_agent(id, "htop".to_string(), false, None)
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
        .install_guest_agent(id, "htop".to_string(), false, None)
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
        .install_guest_agent(id, "htop".to_string(), true, None)
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
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;

    let err = daemon.guest_provision(id, vec![]).await.unwrap_err();
    assert!(matches!(err, DaemonError::InvalidConfig(_)), "err: {err}");
}

#[tokio::test]
async fn running_install_runs_as_supervisor_operation() {
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

    daemon
        .install_guest_agent(id, "htop".to_string(), false, None)
        .await
        .unwrap();

    assert_eq!(mock.installs.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        mock.stops.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no VM lifecycle for a running install"
    );
    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(
        handle.state(),
        InstanceState::Running,
        "running install leaves the VM running"
    );
    assert!(
        handle.active_operation().await.unwrap().is_none(),
        "operation must be finished after the install returns"
    );
}

/// Backend whose guest-agent install blocks on a gate until the test
/// releases it, so an operation can be held in-flight to exercise the
/// supervisor's idempotency-key join.
struct GateBackend {
    installs: AtomicUsize,
    removes: AtomicUsize,
    spawns: AtomicUsize,
    release: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
    spawn_block: Option<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>>,
}

impl GateBackend {
    fn new(release: tokio::sync::mpsc::UnboundedReceiver<()>) -> Self {
        GateBackend {
            installs: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            spawns: AtomicUsize::new(0),
            release: tokio::sync::Mutex::new(release),
            spawn_block: None,
        }
    }

    /// A backend whose `spawn` blocks on a second gate as well, so a test
    /// can hold an auto-start mid-boot with the instance verifiably
    /// `Starting` while its operation is already active.
    fn blocking_spawn(
        release: tokio::sync::mpsc::UnboundedReceiver<()>,
        spawn_block: tokio::sync::mpsc::UnboundedReceiver<()>,
    ) -> Self {
        GateBackend {
            installs: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            spawns: AtomicUsize::new(0),
            release: tokio::sync::Mutex::new(release),
            spawn_block: Some(tokio::sync::Mutex::new(spawn_block)),
        }
    }
}

#[async_trait::async_trait]
impl andler_core::HypervisorBackend for GateBackend {
    fn name(&self) -> &'static str {
        "gate-mock"
    }

    fn supported_render_backends(&self) -> &[andler_core::RenderBackend] {
        &[]
    }

    async fn spawn(&self, _cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        if let Some(spawn_block) = &self.spawn_block {
            let _ = spawn_block.lock().await.recv().await;
        }
        Ok(BackendHandle("gate-mock:vm".to_string()))
    }

    async fn pause(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
        Ok(())
    }

    async fn resume(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
        Ok(())
    }

    async fn stop(&self, _handle: &BackendHandle, _graceful: bool) -> Result<(), BackendError> {
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
        Ok(true)
    }

    async fn guest_exec_install(
        &self,
        _handle: &BackendHandle,
        _package: &str,
    ) -> Result<(), BackendError> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        // Hold the operation in-flight until the test releases the gate.
        let _ = self.release.lock().await.recv().await;
        Ok(())
    }

    async fn guest_exec_remove(
        &self,
        _handle: &BackendHandle,
        _package: &str,
    ) -> Result<(), BackendError> {
        self.removes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn guest_exec_command(
        &self,
        _handle: &BackendHandle,
        _argv: &[String],
        _timeout: Option<std::time::Duration>,
    ) -> Result<andler_core::GuestExecOutput, BackendError> {
        Err(BackendError::NotImplemented {
            backend: "gate-mock",
            operation: "guest_exec_command",
        })
    }

    async fn guest_mutator(
        &self,
        _handle: &BackendHandle,
    ) -> Result<Box<dyn GuestMutator>, BackendError> {
        Err(BackendError::NotImplemented {
            backend: "gate-mock",
            operation: "guest_mutator",
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

/// A stopped instance whose backend blocks `guest_exec_install` on a gate,
/// plus the sender used to release it.
async fn stopped_instance_with_gate_backend(
    dir: &std::path::Path,
) -> (
    Arc<Daemon>,
    Arc<GateBackend>,
    tokio::sync::mpsc::UnboundedSender<()>,
    InstanceId,
) {
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel();
    let mock = Arc::new(GateBackend::new(release_rx));
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;
    daemon
        .backends
        .insert(andler_core::BackendKind::Qemu, mock.clone());
    (Arc::new(daemon), mock, release_tx, id)
}

/// Waits until the instance has an active operation (bounded so a hang
/// reports as a test failure, never a deadlock).
async fn wait_for_active_operation(daemon: &Arc<Daemon>, id: InstanceId) {
    let handle = daemon.handle_for(id).await.unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while handle.active_operation().await.unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the operation never registered as active"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Waits until the gate backend has been entered `n` times (bounded so a
/// stall reports as a test failure, never a deadlock). Reaching the second
/// install is the observable proof that a same-key retry started fresh.
async fn wait_for_installs(mock: &Arc<GateBackend>, n: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while mock.installs.load(Ordering::SeqCst) < n {
        assert!(
            std::time::Instant::now() < deadline,
            "install #{n} never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// A stopped instance whose backend blocks both `spawn` and
/// `guest_exec_install`, so a test can hold the maintenance auto-start
/// mid-boot: the instance is verifiably `Starting` while the operation is
/// active, which is exactly the interleaving the state-gate-only tests miss.
async fn stopped_instance_with_blocking_spawn(
    dir: &std::path::Path,
) -> (
    Arc<Daemon>,
    Arc<GateBackend>,
    tokio::sync::mpsc::UnboundedSender<()>,
    tokio::sync::mpsc::UnboundedSender<()>,
    InstanceId,
) {
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel();
    let (spawn_tx, spawn_rx) = tokio::sync::mpsc::unbounded_channel();
    let mock = Arc::new(GateBackend::blocking_spawn(release_rx, spawn_rx));
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;
    daemon
        .backends
        .insert(andler_core::BackendKind::Qemu, mock.clone());
    (Arc::new(daemon), mock, spawn_tx, release_tx, id)
}

/// Waits until the instance reaches `want` (bounded, so a stall reports as
/// a test failure instead of a deadlock).
async fn wait_for_state(daemon: &Arc<Daemon>, id: InstanceId, want: InstanceState) {
    let handle = daemon.handle_for(id).await.unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while handle.state() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "the instance never reached {want:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// A retry that arrives while the maintenance auto-start is mid-boot must
/// follow the idempotency rule, not the instance-state gate: a same-key
/// retry joins the in-flight operation and a different key is refused.
#[tokio::test]
async fn retry_during_maintenance_boot_follows_the_accept_rule() {
    let dir = TestTempDir::new();
    let (daemon, mock, spawn_release, release, id) =
        stopped_instance_with_blocking_spawn(dir.path()).await;
    let handle = daemon.handle_for(id).await.unwrap();

    let first = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, Some("tok-A".to_string()))
                .await
        }
    });
    wait_for_active_operation(&daemon, id).await;
    wait_for_state(&daemon, id, InstanceState::Starting).await;

    let joined = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, Some("tok-A".to_string()))
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !joined.is_finished(),
        "a same-key retry during the maintenance boot must wait for the install it joined"
    );

    let busy = daemon
        .install_guest_agent(id, "htop".to_string(), false, Some("tok-B".to_string()))
        .await;
    assert!(
        matches!(busy, Err(DaemonError::OperationAlreadyRunning { .. })),
        "a different-key retry during the maintenance boot must be refused, got: {busy:?}"
    );

    // Let the boot finish, the install run, and the VM stop again.
    let _ = spawn_release.send(());
    let _ = release.send(());
    let joined_result = tokio::time::timeout(std::time::Duration::from_secs(5), joined)
        .await
        .expect("the joined retry returns once the install finishes")
        .expect("the joined task must not panic");
    assert!(
        joined_result.is_ok(),
        "the joined retry must report the install's outcome, got: {joined_result:?}"
    );
    let first_result = first
        .await
        .expect("the maintenance install task runs to completion");
    assert!(
        first_result.is_ok(),
        "the maintenance install must succeed, got: {first_result:?}"
    );

    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        1,
        "only the first install ran; the same-key retry joined it"
    );
    assert_eq!(
        handle.state(),
        InstanceState::Stopped,
        "the auto-started maintenance VM is stopped again"
    );
    assert!(
        handle.active_operation().await.unwrap().is_none(),
        "the operation must be finished after the install returns"
    );
}

/// A network retry that carries the SAME idempotency token joins the
/// in-flight operation instead of starting a second one.
#[tokio::test]
async fn install_with_same_idempotency_token_joins_in_flight_operation() {
    let dir = TestTempDir::new();
    let (daemon, mock, release, id) = stopped_instance_with_gate_backend(dir.path()).await;

    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let daemon_first = daemon.clone();
    let first = tokio::spawn(async move {
        let r = daemon_first
            .install_guest_agent(id, "htop".to_string(), false, Some("tok-A".to_string()))
            .await;
        let _ = result_tx.send(r);
    });
    wait_for_active_operation(&daemon, id).await;

    // A retry with the same token joins the in-flight install — and a join
    // waits for the work it joined, so it cannot return while the gate holds
    // the install.
    let retry = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, Some("tok-A".to_string()))
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !retry.is_finished(),
        "a joined retry must wait for the install it joined"
    );

    let _ = release.send(());
    let retry_result = tokio::time::timeout(std::time::Duration::from_secs(5), retry)
        .await
        .expect("the joined retry returns once the install finishes")
        .expect("the retry task must not panic");
    assert!(
        retry_result.is_ok(),
        "same-token retry must join the in-flight install, got: {retry_result:?}"
    );
    first
        .await
        .expect("the first install must complete once the gate is released");
    let first_result = result_rx.await.expect("result channel stays open");
    assert!(
        first_result.is_ok(),
        "the first install must succeed, got: {first_result:?}"
    );

    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        1,
        "only one install must run; the retry joined the first"
    );
    let handle = daemon.handle_for(id).await.unwrap();
    assert_eq!(
        handle.state(),
        InstanceState::Stopped,
        "the auto-started maintenance VM is stopped again"
    );
}

/// A network retry that carries a DIFFERENT idempotency token is refused
/// with OperationAlreadyRunning, because the join key differs.
#[tokio::test]
async fn install_with_different_idempotency_token_is_refused() {
    let dir = TestTempDir::new();
    let (daemon, mock, release, id) = stopped_instance_with_gate_backend(dir.path()).await;

    let daemon_first = daemon.clone();
    let first = tokio::spawn(async move {
        daemon_first
            .install_guest_agent(id, "htop".to_string(), false, Some("tok-A".to_string()))
            .await
    });
    wait_for_active_operation(&daemon, id).await;

    let retry = daemon
        .install_guest_agent(id, "htop".to_string(), false, Some("tok-B".to_string()))
        .await;
    assert!(
        matches!(retry, Err(DaemonError::OperationAlreadyRunning { .. })),
        "a different-token retry must be refused, got: {retry:?}"
    );

    let _ = release.send(());
    first
        .await
        .expect("the install task must not panic")
        .expect("the first install must complete once the gate is released");

    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        1,
        "only one install must run; the different-token retry was refused"
    );
}

/// A RUNNING instance whose backend blocks `guest_exec_install` on a gate,
/// plus the sender used to release it. Exercises the online guest-exec path
/// (running VM), which the stopped-instance join test does not reach.
async fn running_instance_with_gate_backend(
    dir: &std::path::Path,
) -> (
    Arc<Daemon>,
    Arc<GateBackend>,
    tokio::sync::mpsc::UnboundedSender<()>,
    InstanceId,
) {
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    std::fs::write(&cfg.disk.path, b"x").unwrap();
    let id = cfg.id;
    let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel();
    let mock = Arc::new(GateBackend::new(release_rx));
    register_with_state(
        &daemon,
        cfg,
        InstanceState::Running,
        Some(BackendHandle("gate-mock:vm".to_string())),
    )
    .await;
    daemon
        .backends
        .insert(andler_core::BackendKind::Qemu, mock.clone());
    (Arc::new(daemon), mock, release_tx, id)
}

/// Two concurrent online installs (running VM, no idempotency token) must
/// not both run: the second joins the first via the shared op_id key.
#[tokio::test]
async fn two_concurrent_online_installs_run_once() {
    let dir = TestTempDir::new();
    let (daemon, mock, release, id) = running_instance_with_gate_backend(dir.path()).await;

    let first = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, None)
                .await
        }
    });
    wait_for_active_operation(&daemon, id).await;

    let second = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, None)
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !second.is_finished(),
        "the second concurrent install joins the first and waits for it"
    );

    let _ = release.send(());
    let second_result = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("the joined install returns once the first finishes")
        .expect("the second task must not panic");
    assert!(
        second_result.is_ok(),
        "the second concurrent install must join the first, got: {second_result:?}"
    );
    let first_result = first
        .await
        .expect("the first install task runs to completion");
    assert!(
        first_result.is_ok(),
        "the joined install must succeed once the gate is released: {first_result:?}"
    );

    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        1,
        "only one install must run; the second joined the first"
    );
}

/// A guest install cancelled mid-flight (still in-flight, blocked on the
/// gate) must not be silently joined by a same-key retry: the retry has to
/// run a fresh install, because the cancelled op never completes its work.
#[tokio::test]
async fn cancelled_online_install_retry_runs_fresh() {
    let dir = TestTempDir::new();
    let (daemon, mock, release, id) = running_instance_with_gate_backend(dir.path()).await;
    let handle = daemon.handle_for(id).await.unwrap();
    // Hold the first install in-flight on the gate.
    let first = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, None)
                .await
        }
    });
    wait_for_active_operation(&daemon, id).await;
    // Cancel the in-flight op; it stays in-flight until the gate releases.
    let cancelled = handle
        .cancel_operation("guest-install-htop")
        .await
        .expect("cancel must reach the in-flight install operation");
    assert!(cancelled);

    // Retry with the same (no-token) key while the cancelled op is still
    // unwinding: it must start a fresh operation, not join the cancelled
    // one. Spawned concurrently — the fresh install blocks on the same gate.
    let retry = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, None)
                .await
        }
    });
    wait_for_installs(&mock, 2).await;

    // Release the cancelled op: it unwinds on its own schedule, and the
    // fresh install must still be the active one afterwards — a third
    // same-key request joins it instead of starting a third install.
    let _ = release.send(());

    let first_result = first
        .await
        .expect("the cancelled install task runs to its cancellation");
    assert!(
        matches!(first_result, Err(DaemonError::OperationCancelled(_))),
        "the cancelled op must report cancellation, got: {first_result:?}"
    );

    // Let the supervisor observe the superseded op's completion before the
    // next request: without the per-start generation token that is exactly
    // where the fresh install gets mistaken for a finished one.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let joined = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon
                .install_guest_agent(id, "htop".to_string(), false, None)
                .await
        }
    });

    // A join waits for the work it joined, so the third request must still be
    // pending while the fresh install is held on the gate — and it must not
    // have started an install of its own.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !joined.is_finished(),
        "a joined request must wait for the install it joined"
    );
    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        2,
        "the third same-key request must join the running install, never start a third"
    );

    // Release the fresh install: the owner and the joined request both finish.
    let _ = release.send(());
    let joined_result = tokio::time::timeout(std::time::Duration::from_secs(5), joined)
        .await
        .expect("the joined request returns once the install finishes");
    assert!(
        joined_result.is_ok(),
        "the third same-key request must join, got: {joined_result:?}"
    );

    let retry_result = retry
        .await
        .expect("the fresh install task runs to completion");
    assert!(
        retry_result.is_ok(),
        "the same-key retry must run a fresh install, got: {retry_result:?}"
    );

    assert_eq!(
        mock.installs.load(Ordering::SeqCst),
        2,
        "a cancelled op's retry must run a fresh install, not join the cancelled op"
    );
}
