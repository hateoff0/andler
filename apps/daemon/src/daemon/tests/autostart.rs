use super::common::*;
use super::*;

/// Waits up to `timeout` for the instance to reach a state matching
/// `predicate`, asserting the wait is bounded (no unbounded polls).
async fn wait_for_state(
    daemon: &Daemon,
    id: InstanceId,
    predicate: impl Fn(InstanceState) -> bool,
    timeout: std::time::Duration,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let status = daemon.status(id).await.unwrap();
        let state = status.state;
        if predicate(state.clone()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "instance {id} did not reach expected state within {timeout:?}; last: {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Seeds an instance.toml for restore, with `autostart` as given and a
/// disk path that does NOT exist, so a start attempt fails fast at
/// validate_instance_files (no QEMU spawn, deterministic in unit tests).
async fn seed_toml_with_autostart(dir: &std::path::Path, autostart: bool) -> InstanceId {
    let mut cfg = sample_config();
    cfg.disk.path = dir.join("missing-disk.qcow2");
    cfg.firmware.ovmf_vars_path = dir.join("missing-VARS.fd");
    cfg.autostart = autostart;
    let instance_dir = dir.join(cfg.id.to_string());
    tokio::fs::create_dir_all(&instance_dir).await.unwrap();
    super::types::write_instance_toml(&instance_dir, &cfg).await;
    cfg.id
}

#[tokio::test]
async fn restore_attempts_start_for_autostart_marked_instance_and_lands_in_error() {
    let root = TestTempDir::new();
    let id = seed_toml_with_autostart(root.path(), true).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    // The start was attempted (missing disk → fail fast → Error), proving
    // the instance did not stay Stopped.
    wait_for_state(
        &daemon,
        id,
        |state| matches!(state, InstanceState::Error { .. }),
        std::time::Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn restore_does_not_retry_autostart_after_failure() {
    let root = TestTempDir::new();
    let id = seed_toml_with_autostart(root.path(), true).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    wait_for_state(
        &daemon,
        id,
        |state| matches!(state, InstanceState::Error { .. }),
        std::time::Duration::from_secs(5),
    )
    .await;

    // No retry loop: a second window must not flip the instance back to
    // Starting (which a looping retry would do between attempts).
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let status = daemon.status(id).await.unwrap();
    assert!(
        matches!(status.state, InstanceState::Error { .. }),
        "autostart must not be retried; instance flipped back to {:?}",
        status.state
    );
}

#[tokio::test]
async fn restore_leaves_non_autostart_instance_stopped() {
    let root = TestTempDir::new();
    let id = seed_toml_with_autostart(root.path(), false).await;
    let store = andler_store::Store::open_in_memory().await.unwrap();

    let daemon = Daemon::restore_with_root(store, root.path().to_path_buf())
        .await
        .unwrap();

    // Give any (wrong) deferred start a chance to run, then assert the
    // instance was never touched.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let status = daemon.status(id).await.unwrap();
    assert_eq!(status.state, InstanceState::Stopped);
}

#[tokio::test]
async fn autostart_skips_instances_not_in_stopped_state() {
    let daemon = Daemon::new();

    // Already-running instance with autostart=true: must NOT be started
    // again (a second spawn would be a supervisor violation).
    let mut cfg_running = sample_config();
    cfg_running.autostart = true;
    cfg_running.disk.path = std::path::PathBuf::from("/nonexistent-running-disk.qcow2");
    let id_running = cfg_running.id;
    register_with_state(&daemon, cfg_running, InstanceState::Running, None).await;

    // Stopped autostart=true instance: gets the start attempt (fails fast).
    let mut cfg_stopped = sample_config();
    cfg_stopped.autostart = true;
    cfg_stopped.disk.path = std::path::PathBuf::from("/nonexistent-stopped-disk.qcow2");
    let id_stopped = cfg_stopped.id;
    register_with_state(&daemon, cfg_stopped, InstanceState::Stopped, None).await;

    daemon.spawn_autostart_tasks().await;

    wait_for_state(
        &daemon,
        id_stopped,
        |state| matches!(state, InstanceState::Error { .. }),
        std::time::Duration::from_secs(5),
    )
    .await;

    let running = daemon.status(id_running).await.unwrap();
    assert_eq!(
        running.state,
        InstanceState::Running,
        "autostart must skip instances that are already running"
    );
}
