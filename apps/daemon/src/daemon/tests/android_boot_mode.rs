use super::common::*;
use super::*;
use std::path::PathBuf;

#[tokio::test]
async fn switch_android_boot_mode_on_unknown_instance_returns_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .switch_android_boot_mode(InstanceId::new(), andler_core::AndroidBootMode::Linux)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn switch_android_boot_mode_rejects_non_android_instance() {
    let daemon = Daemon::new();
    let id = daemon.create_instance(sample_config()).await.unwrap();

    let err = daemon
        .switch_android_boot_mode(id, andler_core::AndroidBootMode::Linux)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::NotAndroid(_)));
}

#[tokio::test]
async fn switch_android_boot_mode_rejects_running_instance() {
    let daemon = Daemon::new();
    let cfg = sample_android_config(
        PathBuf::from("/tmp/test-android-disk.qcow2"),
        PathBuf::from("/tmp/test-android-base.qcow2"),
    );
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Running, None).await;

    let err = daemon
        .switch_android_boot_mode(id, andler_core::AndroidBootMode::Linux)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DaemonError::InstanceMustBeStopped(_, InstanceState::Running)
    ));
}

#[tokio::test]
async fn get_android_boot_mode_on_unknown_instance_returns_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .get_android_boot_mode(InstanceId::new())
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn get_android_boot_mode_rejects_non_android_instance() {
    let daemon = Daemon::new();
    let id = daemon.create_instance(sample_config()).await.unwrap();

    let err = daemon.get_android_boot_mode(id).await.unwrap_err();
    assert!(matches!(err, DaemonError::NotAndroid(_)));
}

#[tokio::test]
async fn get_android_boot_mode_reads_config_while_running() {
    // P31: boot_mode lives in instance.toml, so `get` must work in any
    // state — the connect level decision needs it on a Running VM and
    // must never require an offline disk mount.
    let daemon = Daemon::new();
    let cfg = sample_android_config(
        PathBuf::from("/tmp/test-android-disk-2.qcow2"),
        PathBuf::from("/tmp/test-android-base-2.qcow2"),
    );
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Running, None).await;

    let mode = daemon.get_android_boot_mode(id).await.unwrap();
    assert_eq!(mode, andler_core::AndroidBootMode::Android);
}
