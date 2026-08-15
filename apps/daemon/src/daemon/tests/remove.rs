use super::common::*;
use super::*;

#[tokio::test]
async fn remove_instance_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .remove_instance(InstanceId::new(), false)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn remove_instance_succeeds_from_created() {
    let daemon = Daemon::new();
    let id = daemon.create_instance(sample_config()).await.unwrap();

    daemon.remove_instance(id, false).await.unwrap();

    let err = daemon.status(id).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn remove_instance_succeeds_from_error_state() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.gpu.render_backend = andler_core::RenderBackend::Passthrough {
        gpu_pci_id: "0000:01:00.0".to_string(),
    };
    let id = daemon.create_instance(cfg).await.unwrap();
    daemon.start_instance(id).await.unwrap_err();

    daemon.remove_instance(id, false).await.unwrap();

    let err = daemon.status(id).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn remove_instance_rejects_running_instance() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Running, None).await;

    let err = daemon.remove_instance(id, false).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::InstanceNotRemovable(_, InstanceState::Running)
    ));

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Running);
}

#[tokio::test]
async fn remove_instance_rejects_every_non_terminal_state() {
    for state in [
        InstanceState::Starting,
        InstanceState::Running,
        InstanceState::Paused,
        InstanceState::Stopping,
    ] {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        register_with_state(&daemon, cfg, state.clone(), None).await;

        let err = daemon.remove_instance(id, false).await.unwrap_err();
        assert!(
            matches!(err, DaemonError::InstanceNotRemovable(_, ref s) if *s == state),
            "expected InstanceNotRemovable({state:?}, ..), got {err:?}"
        );
    }
}

#[tokio::test]
async fn remove_instance_succeeds_from_stopped() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Stopped, None).await;

    daemon.remove_instance(id, false).await.unwrap();

    let err = daemon.status(id).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn remove_instance_without_purge_forgets_registry_but_keeps_disk() {
    let dir = TestTempDir::new();
    let _daemon = Daemon::new();
    let mut cfg = sample_config();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let id = cfg.id;
    super::types::write_instance_toml(dir.path(), &cfg).await;
    let toml_path = dir.path().join("instance.toml");
    assert!(toml_path.exists(), "precondition: toml seeded");

    let daemon = Daemon::new();
    daemon.create_instance(cfg).await.unwrap();
    daemon.remove_instance(id, false).await.unwrap();

    // Registry entry gone: a restart would not resurrect the instance.
    assert!(!toml_path.exists());
    // Disk and firmware files kept for reuse.
    assert!(disk_path.exists());
    assert!(vars_path.exists());
}

#[tokio::test]
async fn remove_instance_with_purge_deletes_instance_toml() {
    let dir = TestTempDir::new();
    let _daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;
    super::types::write_instance_toml(dir.path(), &cfg).await;
    let toml_path = dir.path().join("instance.toml");
    assert!(toml_path.exists());

    let daemon = Daemon::new();
    daemon.create_instance(cfg).await.unwrap();
    daemon.remove_instance(id, true).await.unwrap();

    assert!(!toml_path.exists());
}

#[tokio::test]
async fn remove_instance_disappears_from_list_instances() {
    let daemon = Daemon::new();
    let id = daemon.create_instance(sample_config()).await.unwrap();
    daemon.remove_instance(id, false).await.unwrap();

    let summaries = daemon.list_instances().await;
    assert!(summaries.is_empty());
}

#[tokio::test]
async fn remove_instance_without_purge_leaves_disk_and_firmware_files() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let id = daemon.create_instance(cfg).await.unwrap();

    daemon.remove_instance(id, false).await.unwrap();

    assert!(disk_path.exists());
    assert!(vars_path.exists());
}

#[tokio::test]
async fn remove_instance_with_purge_deletes_disk_and_firmware_files() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let id = daemon.create_instance(cfg).await.unwrap();

    daemon.remove_instance(id, true).await.unwrap();

    assert!(!disk_path.exists());
    assert!(!vars_path.exists());
    assert!(!dir.path().exists());
}

#[tokio::test]
async fn remove_instance_with_purge_keeps_non_empty_parent_directory() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    let unrelated_path = dir.path().join("unrelated-user-file.txt");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();
    tokio::fs::write(&unrelated_path, b"mine").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let registry_dir = dir.path().join("registry");
    let id = daemon
        .create_instance_in(cfg, registry_dir.clone())
        .await
        .unwrap();

    daemon.remove_instance(id, true).await.unwrap();

    assert!(!disk_path.exists());
    assert!(!vars_path.exists());
    assert!(!registry_dir.exists());
    assert!(dir.path().exists());
    assert!(unrelated_path.exists());
}

#[tokio::test]
async fn remove_instance_with_purge_recursively_deletes_own_instance_directory() {
    let root = TestTempDir::new();
    let id = InstanceId::new();
    let instance_dir = root.path().join(id.to_string());
    tokio::fs::create_dir_all(&instance_dir).await.unwrap();

    let disk_path = instance_dir.join("disk.qcow2");
    let vars_path = instance_dir.join("VARS.fd");
    let qemu_log_path = instance_dir.join("qemu.log");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();
    tokio::fs::write(&qemu_log_path, b"[stdout] hi\n")
        .await
        .unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.id = id;
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    daemon.create_instance(cfg).await.unwrap();

    daemon.remove_instance(id, true).await.unwrap();

    assert!(!instance_dir.exists());
}

#[tokio::test]
async fn remove_instance_with_purge_never_deletes_shared_base_image_or_ovmf_code() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("overlay.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    let base_image_path = dir.path().join("base.qcow2");
    let ovmf_code_path = dir.path().join("OVMF_CODE.fd");
    for path in [&disk_path, &vars_path, &base_image_path, &ovmf_code_path] {
        tokio::fs::write(path, b"data").await.unwrap();
    }

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = disk_path.clone();
    cfg.disk.base_image = Some(base_image_path.clone());
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    cfg.firmware.ovmf_code_path = ovmf_code_path.clone();
    let registry_dir = dir.path().join("registry");
    let id = daemon
        .create_instance_in(cfg, registry_dir.clone())
        .await
        .unwrap();

    daemon.remove_instance(id, true).await.unwrap();

    assert!(!disk_path.exists());
    assert!(!vars_path.exists());
    assert!(!registry_dir.exists());
    assert!(base_image_path.exists());
    assert!(ovmf_code_path.exists());
}

#[tokio::test]
async fn remove_instance_with_purge_tolerates_already_missing_files() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.disk.path = disk_path;
    cfg.firmware.ovmf_vars_path = vars_path;
    let id = daemon.create_instance(cfg).await.unwrap();

    daemon.remove_instance(id, true).await.unwrap();

    let err = daemon.status(id).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}
