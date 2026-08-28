use super::common::*;
use super::*;
use andler_core::{InstanceConfig, MemoryConfig};

/// Distinct disk paths per instance so the disk-conflict gate never fires
/// before the memory gate.
fn distinct_disks(cfg_a: &mut InstanceConfig, cfg_b: &mut InstanceConfig) {
    cfg_a.disk.path = std::path::PathBuf::from("/tmp/mem-a.qcow2");
    cfg_b.disk.path = std::path::PathBuf::from("/tmp/mem-b.qcow2");
    let _ = std::fs::File::create(&cfg_a.disk.path);
    let _ = std::fs::File::create(&cfg_b.disk.path);
}

#[tokio::test]
async fn start_refuses_instance_when_running_uses_more_than_host_ram() {
    let daemon = Daemon::new();

    // The running instance already claims more memory than any real host has.
    // Combined with the second instance it must be refused regardless of how
    // the gate reads host RAM (real /proc/meminfo or its fallback).
    let mut cfg_a = sample_config();
    cfg_a.memory.size_bytes = 4096 * MemoryConfig::GIB;
    let mut cfg_b = sample_config(); // default 8 GiB
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let id_a = register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("a second instance beyond host RAM must be refused");
    assert!(
        matches!(err, DaemonError::MemoryOvercommit { .. }),
        "got: {err:?}"
    );
    let _ = daemon.stop_instance(id_a, false).await;
}

#[tokio::test]
async fn start_allows_instance_within_host_ram() {
    let daemon = Daemon::new();

    let cfg = sample_config(); // 8 GiB, well within any host

    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    daemon
        .start_instance(id)
        .await
        .expect("an instance within host RAM must start");
    daemon
        .stop_instance(id, false)
        .await
        .expect("stop the test VM again");
}
