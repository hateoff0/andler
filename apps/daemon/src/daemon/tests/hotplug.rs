use super::common::*;
use super::*;
use andler_core::{
    BackendError, BackendHandle, DiskConfig, NatBackend, NetworkConfig, NetworkMode, RenderBackend,
};

struct MockBackend {
    attached_disks: tokio::sync::Mutex<Vec<(BackendHandle, DiskConfig, usize)>>,
    attached_nets: tokio::sync::Mutex<Vec<(BackendHandle, NetworkConfig, usize)>>,
    fail_attach: std::sync::atomic::AtomicBool,
}

impl MockBackend {
    fn new() -> Self {
        MockBackend {
            attached_disks: tokio::sync::Mutex::new(Vec::new()),
            attached_nets: tokio::sync::Mutex::new(Vec::new()),
            fail_attach: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn disks(&self) -> Vec<DiskConfig> {
        self.attached_disks
            .lock()
            .await
            .iter()
            .map(|(_, d, _)| d.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl andler_core::HypervisorBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn supported_render_backends(&self) -> &[RenderBackend] {
        &[]
    }

    async fn spawn(
        &self,
        _cfg: &andler_core::InstanceConfig,
    ) -> Result<BackendHandle, BackendError> {
        Ok(BackendHandle("mock:instance".to_string()))
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
            state: andler_core::InstanceState::Running,
            detail: None,
            clean_shutdown: false,
        })
    }

    async fn attach_disk(
        &self,
        handle: &BackendHandle,
        disk: &DiskConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        if self.fail_attach.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(BackendError::InvalidConfig {
                backend: "mock",
                reason: "mock attach failure".to_string(),
            });
        }
        self.attached_disks
            .lock()
            .await
            .push((handle.clone(), disk.clone(), index));
        Ok(())
    }

    async fn detach_disk(&self, handle: &BackendHandle, index: usize) -> Result<(), BackendError> {
        self.attached_disks
            .lock()
            .await
            .retain(|(_, _, i)| *i != index);
        let _ = handle;
        Ok(())
    }

    async fn attach_network(
        &self,
        handle: &BackendHandle,
        network: &NetworkConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        if self.fail_attach.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(BackendError::InvalidConfig {
                backend: "mock",
                reason: "mock attach failure".to_string(),
            });
        }
        self.attached_nets
            .lock()
            .await
            .push((handle.clone(), network.clone(), index));
        Ok(())
    }

    async fn detach_network(
        &self,
        handle: &BackendHandle,
        index: usize,
    ) -> Result<(), BackendError> {
        self.attached_nets
            .lock()
            .await
            .retain(|(_, _, i)| *i != index);
        let _ = handle;
        Ok(())
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

async fn running_instance_with_mock(
    dir: &std::path::Path,
) -> (Daemon, std::sync::Arc<MockBackend>, andler_core::InstanceId) {
    let mut daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("disk.qcow2");
    let id = cfg.id;
    let mock = std::sync::Arc::new(MockBackend::new());
    let handle = BackendHandle("qemu:mock-id".to_string());
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Running,
            handle: Some(handle),
        },
    );
    daemon.backends.insert(BackendKind::Qemu, mock.clone());
    (daemon, mock, id)
}

#[tokio::test]
async fn attach_disk_creates_image_and_appends_to_config() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    let cfg = daemon
        .instances
        .read()
        .await
        .get(&id)
        .unwrap()
        .config
        .clone();

    let (path, index) = daemon
        .attach_disk(id, None, 2 * DiskConfig::GIB)
        .await
        .unwrap();
    assert_eq!(index, 0);
    assert_eq!(
        path,
        cfg.disk.path.parent().unwrap().join("disk-extra0.qcow2")
    );
    assert!(path.exists(), "new extra disk image must be created");

    let disks = mock.disks().await;
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0].path, path);
    assert_eq!(disks[0].size_bytes, 2 * DiskConfig::GIB);

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert_eq!(record.config.extra_disks.len(), 1);
}

#[tokio::test]
async fn attach_disk_rejects_zero_size() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let err = daemon.attach_disk(id, None, 0).await.unwrap_err();
    assert!(matches!(err, DaemonError::InvalidConfig(_)));
}

#[tokio::test]
async fn attach_disk_requires_running_or_paused_state() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Created,
            handle: None,
        },
    );
    let err = daemon
        .attach_disk(id, None, 1 * DiskConfig::GIB)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DaemonError::HotplugRequiresRunningInstance(..)
    ));
}

#[tokio::test]
async fn attach_disk_rejects_duplicate_path() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let path = dir.path().join("disk-extra0.qcow2");
    daemon
        .attach_disk(id, None, 1 * DiskConfig::GIB)
        .await
        .unwrap();
    let err = daemon
        .attach_disk(id, Some(path.clone()), 1 * DiskConfig::GIB)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::DiskAlreadyAttached(_, _)));

    let another = dir.path().join("elsewhere.qcow2");
    daemon
        .attach_disk(id, Some(another.clone()), 1 * DiskConfig::GIB)
        .await
        .unwrap();
    let err = daemon
        .attach_disk(id, Some(another), 1 * DiskConfig::GIB)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::DiskAlreadyAttached(_, _)));
}

#[tokio::test]
async fn attach_disk_rolls_back_created_file_on_backend_failure() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    mock.fail_attach
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let err = daemon
        .attach_disk(id, None, 1 * DiskConfig::GIB)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::Backend(_)));
    assert!(
        !dir.path().join("disk-extra0.qcow2").exists(),
        "created image must be removed when the backend attach fails"
    );
    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert!(record.config.extra_disks.is_empty());
}

#[tokio::test]
async fn attach_disk_existing_image_uses_virtual_size() {
    let dir = TestTempDir::new();
    let existing = dir.path().join("premade.qcow2");
    andler_disk::qcow2::create(&existing, 4 * DiskConfig::GIB)
        .await
        .unwrap();

    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    let (path, _) = daemon
        .attach_disk(id, Some(existing.clone()), 0)
        .await
        .unwrap();
    assert_eq!(path, existing);
    let disks = mock.disks().await;
    assert_eq!(disks[0].size_bytes, 4 * DiskConfig::GIB);
}

#[tokio::test]
async fn detach_disk_removes_config_entry_but_keeps_file() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let (path, _) = daemon
        .attach_disk(id, None, 1 * DiskConfig::GIB)
        .await
        .unwrap();
    daemon.detach_disk(id, path.clone()).await.unwrap();

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert!(record.config.extra_disks.is_empty());
    assert!(
        path.exists(),
        "detach must not delete the user's disk image"
    );
}

#[tokio::test]
async fn detach_disk_unknown_path_returns_not_attached() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let err = daemon
        .detach_disk(id, dir.path().join("ghost.qcow2"))
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::DiskNotAttached(_, _)));
}

#[tokio::test]
async fn detach_disk_respects_state_gate() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.instances.write().await.insert(
        id,
        InstanceRecord {
            config: cfg,
            state: InstanceState::Stopped,
            handle: None,
        },
    );
    let err = daemon
        .detach_disk(id, std::path::PathBuf::from("/tmp/x.qcow2"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DaemonError::HotplugRequiresRunningInstance(..)
    ));
}

#[tokio::test]
async fn attach_network_appends_to_config() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    let net = NetworkConfig {
        mode: NetworkMode::Nat,
        device_model: "virtio-net-pci".to_string(),
        nat_backend: NatBackend::Slirp,
    };
    let index = daemon.attach_network(id, net.clone()).await.unwrap();
    assert_eq!(index, 0);
    assert_eq!(mock.attached_nets.lock().await.len(), 1);
    {
        let instances = daemon.instances.read().await;
        let record = instances.get(&id).unwrap();
        assert_eq!(record.config.extra_networks, vec![net]);
    }

    let net2 = NetworkConfig {
        mode: NetworkMode::Isolated,
        device_model: "virtio-net-pci".to_string(),
        nat_backend: NatBackend::Slirp,
    };
    let index = daemon.attach_network(id, net2).await.unwrap();
    assert_eq!(index, 1);
}

#[tokio::test]
async fn attach_network_rejects_empty_model() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let net = NetworkConfig {
        mode: NetworkMode::Nat,
        device_model: String::new(),
        nat_backend: NatBackend::Slirp,
    };
    let err = daemon.attach_network(id, net).await.unwrap_err();
    assert!(matches!(err, DaemonError::InvalidConfig(_)));
}

#[tokio::test]
async fn detach_network_removes_by_index() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    let net = NetworkConfig {
        mode: NetworkMode::Nat,
        device_model: "virtio-net-pci".to_string(),
        nat_backend: NatBackend::Slirp,
    };
    daemon.attach_network(id, net.clone()).await.unwrap();
    let net2 = NetworkConfig {
        mode: NetworkMode::Bridge {
            interface: "br0".to_string(),
        },
        device_model: "e1000e".to_string(),
        nat_backend: NatBackend::Slirp,
    };
    daemon.attach_network(id, net2).await.unwrap();

    daemon.detach_network(id, 0).await.unwrap();
    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert_eq!(record.config.extra_networks.len(), 1);
    assert_eq!(
        record.config.extra_networks[0].mode,
        NetworkMode::Bridge {
            interface: "br0".to_string()
        }
    );
    assert_eq!(mock.attached_nets.lock().await.len(), 1);
}

#[tokio::test]
async fn detach_network_out_of_range_returns_network_not_attached() {
    let dir = TestTempDir::new();
    let (daemon, _mock, id) = running_instance_with_mock(dir.path()).await;
    let err = daemon.detach_network(id, 3).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::NetworkNotAttached {
            index: 3,
            attached: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn attach_disk_detaches_propagate_backend_errors() {
    let dir = TestTempDir::new();
    let (daemon, mock, id) = running_instance_with_mock(dir.path()).await;
    mock.fail_attach
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let err = daemon
        .attach_network(
            id,
            NetworkConfig {
                mode: NetworkMode::Nat,
                device_model: "virtio-net-pci".to_string(),
                nat_backend: NatBackend::Slirp,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::Backend(_)));
    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert!(record.config.extra_networks.is_empty());
}
