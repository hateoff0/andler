use super::common::*;
use super::*;
use andler_core::{AndroidProfile, ArmTranslator, InstanceKind};

#[tokio::test]
async fn create_instance_registers_with_created_state() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;

    let returned_id = daemon.create_instance(cfg).await.unwrap();
    assert_eq!(returned_id, id);

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Created);
}

#[tokio::test]
async fn double_create_with_same_id_overwrites_record() {
    let daemon = Daemon::new();
    let cfg1 = sample_config();
    let id = cfg1.id;
    daemon.create_instance(cfg1).await.unwrap();

    let mut cfg2 = sample_config();
    cfg2.id = id;
    cfg2.name = "renamed".to_string();
    daemon.create_instance(cfg2).await.unwrap();

    let instances = daemon.instances.read().await;
    assert_eq!(instances.get(&id).unwrap().config.name, "renamed");
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn create_linux_instance_places_disk_inside_own_instance_dir() {
    // Regression test: create_linux_instance used to create the qcow2
    // disk at whatever path the caller supplied (e.g. the wizard's flat
    // `<instances_root>/<name>-disk.qcow2`, a sibling of the per-instance
    // directory, not inside it) while VARS.fd always went inside
    // `<instances_root>/<id>/`. That put disk.qcow2 and VARS.fd for the
    // *same* instance in two different directories, silently breaking
    // anything that assumes `disk.path.parent()` is the instance's own
    // directory (qemu.log, purge's recursive delete).
    let dir = std::env::temp_dir().join(format!(
        "andler-daemon-test-linux-disk-path-{}",
        std::process::id()
    ));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();
    let instances_root = dir.join("instances");

    let daemon = Daemon::new();
    let mut cfg = sample_config();
    // Mimic the wizard's old flat naming — the point of the fix is that
    // it doesn't matter what path is passed in here, only the file name
    // is kept.
    cfg.disk.path = instances_root.join("my-vm-disk.qcow2");
    cfg.disk.size_bytes = 1024 * 1024; // keep the test fast

    let id = daemon
        .create_linux_instance(cfg, instances_root.clone(), ovmf_template)
        .await
        .unwrap();

    let instance_dir = instances_root.join(id.0.to_string());
    let expected_disk_path = instance_dir.join("my-vm-disk.qcow2");
    assert!(expected_disk_path.exists());
    assert!(instance_dir.join("VARS.fd").exists());

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert_eq!(record.config.disk.path, expected_disk_path);
    assert_eq!(
        record.config.disk.path.parent(),
        record.config.firmware.ovmf_vars_path.parent(),
        "disk.qcow2 and VARS.fd must live in the same instance directory"
    );

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
#[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
async fn create_android_instance_resolves_profile_and_creates_overlay() {
    let dir = std::env::temp_dir().join("andler-daemon-test-android-e2e");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let base_image = dir.join("base.qcow2");
    andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
        .await
        .unwrap();
    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
        .await
        .unwrap();

    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: true,
        microg: false,
        arm_translator: ArmTranslator::Libndk,
    };

    let id = daemon
        .create_android_instance(
            profile.clone(),
            "my-android".to_string(),
            base_image.clone(),
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
        )
        .await
        .unwrap();

    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Created);

    let instance_dir = instances_root.join(id.0.to_string());
    assert!(instance_dir.join("disk.qcow2").exists());
    assert!(instance_dir.join("VARS.fd").exists());

    // PLAN.md item 5: instance.toml written alongside the disk/VARS.fd,
    // human-readable and round-trippable back into an InstanceConfig
    // (not byte-for-byte identical necessarily, just semantically the
    // same instance).
    let toml_content = tokio::fs::read_to_string(instance_dir.join("instance.toml"))
        .await
        .expect("instance.toml must be written on create_android_instance");
    let parsed: andler_core::InstanceConfig =
        toml::from_str(&toml_content).expect("instance.toml must parse back into InstanceConfig");
    assert_eq!(parsed.id, id);
    assert_eq!(parsed.name, "my-android");

    let instances = daemon.instances.read().await;
    let record = instances.get(&id).unwrap();
    assert_eq!(record.config.id, id);
    assert_eq!(record.config.disk.base_image, Some(base_image));
    match &record.config.kind {
        InstanceKind::AndroidVm { android_profile } => {
            assert_eq!(*android_profile, profile);
        }
        InstanceKind::LinuxVm { .. } => panic!("expected AndroidVm"),
    }

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
async fn create_android_instance_fails_when_base_image_missing() {
    let dir = std::env::temp_dir().join("andler-daemon-test-android-missing-base");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let ovmf_template = dir.join("OVMF_VARS.template.fd");
    tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
        .await
        .unwrap();

    let missing_base = dir.join("does-not-exist.qcow2");
    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: true,
        arm_translator: ArmTranslator::None,
    };

    let err = daemon
        .create_android_instance(
            profile,
            "my-android".to_string(),
            missing_base,
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            ovmf_template,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        DaemonError::Disk(andler_disk::DiskError::BackingFileNotFound(_))
    ));

    let mut instance_dirs = tokio::fs::read_dir(&instances_root).await.unwrap();
    assert!(
        instance_dirs.next_entry().await.unwrap().is_none(),
        "instances_root must be empty after a failed create_android_instance"
    );

    tokio::fs::remove_dir_all(&dir).await.ok();
}

#[tokio::test]
async fn create_android_instance_cleans_up_instance_dir_on_missing_ovmf_template() {
    let dir = std::env::temp_dir().join("andler-daemon-test-android-missing-ovmf");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let missing_ovmf_template = dir.join("does-not-exist-template.fd");
    let base_image = dir.join("base.qcow2");
    let instances_root = dir.join("instances");
    let daemon = Daemon::new();

    let profile = AndroidProfile {
        android_version: AndroidVersion::Android13,
        gapps: false,
        microg: true,
        arm_translator: ArmTranslator::None,
    };

    let err = daemon
        .create_android_instance(
            profile,
            "my-android".to_string(),
            base_image,
            instances_root.clone(),
            20 * 1024 * 1024 * 1024,
            missing_ovmf_template,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DaemonError::Firmware(_)));

    let mut instance_dirs = tokio::fs::read_dir(&instances_root).await.unwrap();
    assert!(
        instance_dirs.next_entry().await.unwrap().is_none(),
        "instances_root must be empty after copy(ovmf_vars_template) fails"
    );

    tokio::fs::remove_dir_all(&dir).await.ok();
}
