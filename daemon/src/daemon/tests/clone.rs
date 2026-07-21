use super::common::*;
use super::*;
use std::path::PathBuf;
use andler_core::{AndroidProfile, ArmTranslator, CloneMode};


#[tokio::test]
async fn clone_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .clone_instance(
            InstanceId::new(),
            "clone".to_string(),
            PathBuf::from("/tmp/instances"),
            CloneMode::Linked,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn clone_rejects_linux_vm_with_shared_base() {
    let daemon = Daemon::new();
    let cfg = sample_config(); // LinuxVm
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            PathBuf::from("/tmp/instances"),
            CloneMode::SharedBase,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::SharedBaseNotSupportedForLinuxVm(returned_id) if returned_id == id));
}

#[tokio::test]
async fn clone_rejects_non_terminal_source_state() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let cfg = sample_android_config(
        dir.path().join("disk.qcow2"),
        dir.path().join("base.qcow2"),
    );
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    {
        let mut instances = daemon.instances.write().await;
        let record = instances.get_mut(&id).unwrap();
        record.state = InstanceState::Running;
    }

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            dir.path().to_path_buf(),
            CloneMode::Linked,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DaemonError::InstanceNotClonable(returned_id, InstanceState::Running)
            if returned_id == id
    ));
}


#[tokio::test]
async fn find_live_clones_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon.find_live_clones(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn find_live_clones_is_empty_when_nothing_references_the_disk() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let cfg = sample_android_config(
        dir.path().join("disk.qcow2"),
        dir.path().join("base.qcow2"),
    );
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let clones = daemon.find_live_clones(id).await.unwrap();
    assert!(clones.is_empty());
}

#[tokio::test]
async fn find_live_clones_finds_instance_whose_base_image_is_the_source_disk() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();

    let source_disk = dir.path().join("source").join("disk.qcow2");
    let source_cfg =
        sample_android_config(source_disk.clone(), dir.path().join("base.qcow2"));
    let source_id = source_cfg.id;
    daemon.create_instance(source_cfg).await.unwrap();

    let linked_clone_cfg =
        sample_android_config(dir.path().join("clone").join("disk.qcow2"), source_disk.clone());
    let clone_id = linked_clone_cfg.id;
    daemon.create_instance(linked_clone_cfg).await.unwrap();

    let clones = daemon.find_live_clones(source_id).await.unwrap();
    assert_eq!(clones, vec![clone_id]);
}

#[tokio::test]
async fn find_live_clones_ignores_instances_sharing_only_the_base_image() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let shared_base_image = dir.path().join("base.qcow2");

    let first_cfg = sample_android_config(
        dir.path().join("first").join("disk.qcow2"),
        shared_base_image.clone(),
    );
    let first_id = first_cfg.id;
    daemon.create_instance(first_cfg).await.unwrap();

    let second_cfg = sample_android_config(
        dir.path().join("second").join("disk.qcow2"),
        shared_base_image,
    );
    daemon.create_instance(second_cfg).await.unwrap();

    let clones = daemon.find_live_clones(first_id).await.unwrap();
    assert!(clones.is_empty());
}

#[tokio::test]
async fn remove_instance_with_purge_rejects_when_live_linked_clone_exists() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("source").join("disk.qcow2");
    let vars_path = dir.path().join("source").join("VARS.fd");
    tokio::fs::create_dir_all(disk_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut source_cfg =
        sample_android_config(disk_path.clone(), dir.path().join("base.qcow2"));
    source_cfg.firmware.ovmf_vars_path = vars_path.clone();
    let source_id = source_cfg.id;
    daemon.create_instance(source_cfg).await.unwrap();

    let clone_cfg =
        sample_android_config(dir.path().join("clone").join("disk.qcow2"), disk_path.clone());
    let clone_id = clone_cfg.id;
    daemon.create_instance(clone_cfg).await.unwrap();

    let err = daemon.remove_instance(source_id, true).await.unwrap_err();
    assert!(matches!(
        err,
        DaemonError::InstanceHasLiveClones(returned_id, ref clones)
            if returned_id == source_id && clones == &vec![clone_id]
    ));

    assert!(disk_path.exists());
    assert!(vars_path.exists());
    assert!(daemon.status(source_id).await.is_ok());
}

#[tokio::test]
async fn remove_instance_with_purge_succeeds_when_clone_is_full_standalone() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("source").join("disk.qcow2");
    let vars_path = dir.path().join("source").join("VARS.fd");
    tokio::fs::create_dir_all(disk_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut source_cfg =
        sample_android_config(disk_path.clone(), dir.path().join("base.qcow2"));
    source_cfg.firmware.ovmf_vars_path = vars_path.clone();
    let source_id = source_cfg.id;
    daemon.create_instance(source_cfg).await.unwrap();

    let mut standalone_clone_cfg =
        sample_android_config(dir.path().join("clone").join("disk.qcow2"), disk_path.clone());
    standalone_clone_cfg.disk.base_image = None;
    daemon.create_instance(standalone_clone_cfg).await.unwrap();

    daemon.remove_instance(source_id, true).await.unwrap();

    assert!(!disk_path.exists());
    assert!(!vars_path.exists());
}


#[tokio::test]
async fn clone_linux_vm_linked_mode_is_allowed() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let id = daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            dir.path().to_path_buf(),
            CloneMode::Linked,
        )
        .await
        .unwrap_err();
    assert!(
        !matches!(
            err,
            DaemonError::SharedBaseNotSupportedForLinuxVm(_)
        ),
        "LinuxVm + Linked should not be blocked, got: {err:?}"
    );
}

#[tokio::test]
async fn clone_linux_vm_full_standalone_mode_is_allowed() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = disk_path.clone();
    cfg.firmware.ovmf_vars_path = vars_path.clone();
    let id = daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            dir.path().to_path_buf(),
            CloneMode::FullStandalone,
        )
        .await
        .unwrap_err();
    assert!(
        !matches!(
            err,
            DaemonError::SharedBaseNotSupportedForLinuxVm(_)
        ),
        "LinuxVm + FullStandalone should not be blocked, got: {err:?}"
    );
}

#[tokio::test]
async fn clone_linux_vm_shared_base_mode_returns_shared_base_not_supported() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = disk_path;
    cfg.firmware.ovmf_vars_path = vars_path;
    let id = daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            dir.path().to_path_buf(),
            CloneMode::SharedBase,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::SharedBaseNotSupportedForLinuxVm(returned_id) if returned_id == id));
}

#[tokio::test]
async fn export_linux_vm_disk_does_not_block_on_instance_kind() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&disk_path, b"disk").await.unwrap();
    tokio::fs::write(&vars_path, b"vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = disk_path;
    cfg.firmware.ovmf_vars_path = vars_path;
    let id = daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .export_instance_disk(id, dir.path().join("exported.qcow2"))
        .await
        .unwrap_err();
    assert!(
        !matches!(
            err,
            DaemonError::SharedBaseNotSupportedForLinuxVm(_)
        ),
        "LinuxVm export should not be blocked, got: {err:?}"
    );
}

#[tokio::test]
async fn clone_linux_vm_rejects_non_terminal_source_state() {
    let dir = TestTempDir::new();
    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = dir.path().join("disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.path().join("VARS.fd");
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    {
        let mut instances = daemon.instances.write().await;
        let record = instances.get_mut(&id).unwrap();
        record.state = InstanceState::Running;
    }

    let err = daemon
        .clone_instance(
            id,
            "clone".to_string(),
            dir.path().to_path_buf(),
            CloneMode::Linked,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DaemonError::InstanceNotClonable(returned_id, InstanceState::Running)
            if returned_id == id
    ));
}


#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn export_linux_vm_disk_creates_standalone_file() {
    let dir = TestTempDir::new();
    let disk_path = dir.path().join("disk.qcow2");
    andler_disk::qcow2::create(&disk_path, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let vars_path = dir.path().join("VARS.fd");
    tokio::fs::write(&vars_path, b"fake-vars").await.unwrap();

    let daemon = Daemon::new();
    let mut cfg = sample_config(); // LinuxVm
    cfg.disk.path = disk_path;
    cfg.firmware.ovmf_vars_path = vars_path;
    let id = daemon.create_instance(cfg).await.unwrap();

    let export_path = dir.path().join("exported.qcow2");
    daemon
        .export_instance_disk(id, export_path.clone())
        .await
        .unwrap();

    assert!(export_path.exists());
    assert!(daemon.status(id).await.is_ok());
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn clone_instance_with_linked_mode_creates_overlay_pointing_at_source_disk() {
    let dir = std::env::temp_dir().join("andler-daemon-test-clone-linked");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: false,
        arm_translator: ArmTranslator::None,
    };
    let source_id = daemon
        .create_android_instance(
            profile,
            "source".to_string(),
            base_image.clone(),
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
                    )
        .await
        .unwrap();
    let source_disk_path = daemon
        .get_instance_config(source_id)
        .await
        .unwrap()
        .disk
        .path;

    let clone_id = daemon
        .clone_instance(
            source_id,
            "clone-of-source".to_string(),
            instances_root.clone(),
            CloneMode::Linked,
        )
        .await
        .unwrap();

    let clone_cfg = daemon.get_instance_config(clone_id).await.unwrap();
    assert_eq!(clone_cfg.name, "clone-of-source");
    assert_eq!(clone_cfg.disk.base_image, Some(source_disk_path.clone()));
    assert!(clone_cfg.disk.path.exists());
    assert_ne!(clone_cfg.disk.path, source_disk_path);
    assert!(clone_cfg.firmware.ovmf_vars_path.exists());
    assert_ne!(
        clone_cfg.firmware.ovmf_vars_path,
        daemon.get_instance_config(source_id).await.unwrap().firmware.ovmf_vars_path
    );

    let err = daemon.remove_instance(source_id, true).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceHasLiveClones(_, _)));

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn clone_instance_with_full_standalone_mode_has_no_base_image() {
    let dir = std::env::temp_dir().join("andler-daemon-test-clone-standalone");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: false,
        arm_translator: ArmTranslator::None,
    };
    let source_id = daemon
        .create_android_instance(
            profile,
            "source".to_string(),
            base_image,
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
                    )
        .await
        .unwrap();

    let clone_id = daemon
        .clone_instance(
            source_id,
            "standalone-clone".to_string(),
            instances_root,
            CloneMode::FullStandalone,
        )
        .await
        .unwrap();

    let clone_cfg = daemon.get_instance_config(clone_id).await.unwrap();
    assert_eq!(clone_cfg.disk.base_image, None);
    assert!(clone_cfg.disk.path.exists());

    daemon.remove_instance(source_id, true).await.unwrap();

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn clone_instance_with_shared_base_mode_survives_source_purge() {
    let dir = std::env::temp_dir().join("andler-daemon-test-clone-shared-base");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: false,
        arm_translator: ArmTranslator::None,
    };
    let source_id = daemon
        .create_android_instance(
            profile,
            "source".to_string(),
            base_image.clone(),
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
                    )
        .await
        .unwrap();

    let clone_id = daemon
        .clone_instance(
            source_id,
            "shared-base-clone".to_string(),
            instances_root,
            CloneMode::SharedBase,
        )
        .await
        .unwrap();

    let clone_cfg_before = daemon.get_instance_config(clone_id).await.unwrap();
    assert_eq!(clone_cfg_before.disk.base_image, Some(base_image));
    assert!(clone_cfg_before.disk.path.exists());

    daemon.remove_instance(source_id, true).await.unwrap();
    assert!(clone_cfg_before.disk.path.exists());

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn clone_instance_of_a_clone_is_allowed() {
    let dir = std::env::temp_dir().join("andler-daemon-test-clone-of-clone");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: false,
        arm_translator: ArmTranslator::None,
    };
    let source_id = daemon
        .create_android_instance(
            profile,
            "source".to_string(),
            base_image,
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
                    )
        .await
        .unwrap();

    let clone_a_id = daemon
        .clone_instance(
            source_id,
            "clone-a".to_string(),
            instances_root.clone(),
            CloneMode::Linked,
        )
        .await
        .unwrap();

    let clone_b_id = daemon
        .clone_instance(
            clone_a_id,
            "clone-b".to_string(),
            instances_root,
            CloneMode::Linked,
        )
        .await
        .unwrap();

    let clone_a_disk = daemon.get_instance_config(clone_a_id).await.unwrap().disk.path;
    let clone_b_cfg = daemon.get_instance_config(clone_b_id).await.unwrap();
    assert_eq!(clone_b_cfg.disk.base_image, Some(clone_a_disk.clone()));

    let err = daemon.remove_instance(clone_a_id, true).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceHasLiveClones(returned_id, ref clones)
        if returned_id == clone_a_id && clones == &vec![clone_b_id]));

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn export_instance_disk_creates_standalone_file_without_registering_instance() {
    let dir = std::env::temp_dir().join("andler-daemon-test-export");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: false,
        arm_translator: ArmTranslator::None,
    };
    let source_id = daemon
        .create_android_instance(
            profile,
            "source".to_string(),
            base_image,
            instances_root,
            20 * 1024 * 1024 * 1024,
            ovmf_template,
                    )
        .await
        .unwrap();

    let export_path = dir.join("exported.qcow2");
    daemon
        .export_instance_disk(source_id, export_path.clone())
        .await
        .unwrap();

    assert!(export_path.exists());

    let instances_before = daemon.list_instances().await;
    assert_eq!(instances_before.len(), 1);
    assert_eq!(instances_before[0].id, source_id);

    tokio::fs::remove_dir_all(&dir).await.ok();
}
