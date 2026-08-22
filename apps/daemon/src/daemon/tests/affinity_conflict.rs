use super::common::*;
use super::*;
use andler_core::InstanceConfig;

/// Distinct disk paths per instance so the disk-conflict gate never fires

/// Distinct disk paths per instance so the disk-conflict gate never fires
/// before the affinity gate; creates the files so a successful start can
/// pass validation.
fn distinct_disks(cfg_a: &mut InstanceConfig, cfg_b: &mut InstanceConfig) {
    cfg_a.disk.path = std::path::PathBuf::from("/tmp/aff-a.qcow2");
    cfg_b.disk.path = std::path::PathBuf::from("/tmp/aff-b.qcow2");
    let _ = std::fs::File::create(&cfg_a.disk.path);
    let _ = std::fs::File::create(&cfg_b.disk.path);
}

#[tokio::test]
async fn start_rejects_cpu_affinity_overlapping_another_running_instance() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.cpu.affinity = Some(vec![0, 1, 2]);
    let mut cfg_b = sample_config();
    cfg_b.cpu.affinity = Some(vec![2, 3]);
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("start with overlapping pinned CPUs must fail");
    assert!(
        matches!(
            err,
            DaemonError::CpuAffinityConflict {
                cpu: 2,
                instance: _,
                held_by,
            } if held_by == id_a
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn start_allows_disjoint_cpu_affinity() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.cpu.affinity = Some(vec![0, 1]);
    let mut cfg_b = sample_config();
    cfg_b.cpu.affinity = Some(vec![4, 5]);
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // No overlap, so the check passes and start proceeds (a real qemu is
    // spawned in this environment); stop it so no VM outlives the test.
    daemon
        .start_instance(id_b)
        .await
        .expect("start with disjoint pinned CPUs must proceed");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}

#[tokio::test]
async fn unpinned_instance_does_not_conflict_with_pinned_one() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.cpu.affinity = Some(vec![0, 1]);
    let mut cfg_b = sample_config();
    cfg_b.cpu.affinity = None;
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // Unpinned instance: the scheduler may use any CPU, but that is not a
    // pin conflict — refusing it would be wrong (it never asked to pin).
    daemon
        .start_instance(id_b)
        .await
        .expect("unpinned start must proceed past the affinity gate");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}

#[tokio::test]
async fn pinned_instance_does_not_conflict_with_unpinned_runner() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.cpu.affinity = None;
    let mut cfg_b = sample_config();
    cfg_b.cpu.affinity = Some(vec![0]);
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // Pinned instance: the other VM is unpinned (scheduler-managed), so
    // the pin is not defeated by an explicit conflicting pin.
    daemon
        .start_instance(id_b)
        .await
        .expect("pinned start must proceed past the affinity gate");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}

#[tokio::test]
async fn stopped_pinned_instance_does_not_hold_cpus() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.cpu.affinity = Some(vec![0, 1]);
    let mut cfg_b = sample_config();
    cfg_b.cpu.affinity = Some(vec![1]);
    distinct_disks(&mut cfg_a, &mut cfg_b);

    let _id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Stopped, None).await;

    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // A stopped instance does not hold its pinned CPUs, so the start
    // proceeds (and stops again so no VM outlives the test).
    daemon
        .start_instance(id_b)
        .await
        .expect("start must proceed past the affinity gate when the holder is stopped");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}

#[tokio::test]
async fn start_rejects_affinity_index_beyond_host_cpu_count() {
    let daemon = Daemon::new();

    let host_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut cfg = sample_config();
    cfg.cpu.affinity = Some(vec![host_cpus]); // one past the last valid index
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let err = daemon
        .start_instance(id)
        .await
        .expect_err("an affinity index at/above the host CPU count must fail validation");
    assert!(matches!(err, DaemonError::InvalidConfig(_)), "got: {err:?}");
}
