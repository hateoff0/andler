use super::common::*;
use super::*;
use std::path::PathBuf;

#[tokio::test]
async fn start_rejects_disk_held_by_another_running_instance() {
    let daemon = Daemon::new();
    let shared_disk = PathBuf::from("/tmp/shared-disk.qcow2");

    let mut cfg_a = sample_config();
    cfg_a.disk.path = shared_disk.clone();
    let id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let mut cfg_b = sample_config();
    cfg_b.disk.path = shared_disk;
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("start with a disk held by another running instance must fail");
    assert!(
        matches!(
            err,
            DaemonError::DiskInUse {
                ref path,
                instance: _,
                held_by,
            } if path.to_string_lossy().ends_with("shared-disk.qcow2") && held_by == id_a
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn start_allows_same_disk_when_other_instance_is_stopped() {
    let daemon = Daemon::new();
    let shared_disk = PathBuf::from("/tmp/shared-disk-stopped.qcow2");

    let mut cfg_a = sample_config();
    cfg_a.disk.path = shared_disk.clone();
    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Stopped, None).await;

    // The disk file must exist for the start to pass validation.
    let _ = std::fs::File::create(&shared_disk);
    let mut cfg_b = sample_config();
    cfg_b.disk.path = shared_disk;
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // No running instance holds the disk, so the check passes and start
    // proceeds (a real qemu is spawned in this environment); stop it again
    // so no VM outlives the test.
    daemon
        .start_instance(id_b)
        .await
        .expect("start without a disk conflict must proceed");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}

#[tokio::test]
async fn start_rejects_extra_disk_held_by_another_running_instance() {
    let daemon = Daemon::new();
    let shared_extra = PathBuf::from("/tmp/shared-extra-disk.qcow2");

    let mut cfg_a = sample_config();
    cfg_a.disk.path = std::path::PathBuf::from("/tmp/a-primary.qcow2");
    cfg_a.extra_disks = vec![{
        let mut d = andler_core::DiskConfig::reference_default(shared_extra.clone());
        d.size_bytes = 1024;
        d
    }];
    let id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let mut cfg_b = sample_config();
    cfg_b.disk.path = std::path::PathBuf::from("/tmp/b-primary.qcow2");
    cfg_b.extra_disks = vec![{
        let mut d = andler_core::DiskConfig::reference_default(shared_extra);
        d.size_bytes = 1024;
        d
    }];
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("start with an extra disk held by another running instance must fail");
    assert!(
        matches!(
            err,
            DaemonError::DiskInUse {
                ref path,
                instance: _,
                held_by,
            } if path.to_string_lossy().ends_with("shared-extra-disk.qcow2") && held_by == id_a
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn shared_base_image_is_not_a_disk_conflict() {
    let daemon = Daemon::new();
    let base = PathBuf::from("/tmp/shared-base.qcow2");

    // Two Android overlays on the same base image: distinct top-level
    // paths, shared read-only backing — must NOT be treated as a conflict.
    let mut cfg_a = sample_android_config(PathBuf::from("/tmp/a-overlay.qcow2"), base.clone());
    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let mut cfg_b = sample_android_config(PathBuf::from("/tmp/b-overlay.qcow2"), base);
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // The check must pass (no DiskInUse); the subsequent start fails on
    // the missing disk file, which proves we got past the conflict gate.
    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("start proceeds past the disk-conflict gate");
    assert!(
        !matches!(err, DaemonError::DiskInUse { .. }),
        "shared base image must not be reported as a disk conflict: {err:?}"
    );
}
