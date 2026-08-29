use super::common::*;
use super::*;

/// Two instances still in `Created` and started concurrently must serialize:
/// the four per-start resource gates only count `Running`/`Paused`/`Starting`
/// peers, so without a lock both would pass the pairwise checks (the peer is
/// only `Created`, never counted) and both spawn against the same host port.
/// The global start lock holds across the check→Start transition, so once the
/// first reaches `Starting` it owns the port and the second's check fails with
/// a clean `PortForwardConflict` instead of an opaque spawn-time lock error.
#[tokio::test]
async fn concurrent_starts_of_same_host_port_instances_serialize_one_wins() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_a = cfg_a.id;
    daemon.create_instance(cfg_a).await.unwrap();

    let mut cfg_b = sample_config();
    cfg_b.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // Both instances are `Created`. Launch both starts concurrently; the
    // pairwise checks alone would let both through — this is the §H
    // multi-instance TOCTOU race the start lock closes.
    let (ra, rb) = tokio::join!(daemon.start_instance(id_a), daemon.start_instance(id_b),);

    let a_ok = ra.is_ok();
    let mut oks = 0;
    match ra {
        Ok(()) => oks += 1,
        Err(e) => assert!(
            matches!(e, DaemonError::PortForwardConflict { .. }),
            "the losing concurrent start must fail with a port conflict, got {e:?}"
        ),
    }
    match rb {
        Ok(()) => oks += 1,
        Err(e) => assert!(
            matches!(e, DaemonError::PortForwardConflict { .. }),
            "the losing concurrent start must fail with a port conflict, got {e:?}"
        ),
    }
    assert_eq!(
        oks, 1,
        "exactly one of two concurrent same-port starts must win"
    );

    // Stop the winner so no VM outlives the test.
    let winner = if a_ok { id_a } else { id_b };
    let _ = daemon.stop_instance(winner, false).await;
}

/// The same race over a shared disk path: the loser must fail with a
/// `DiskInUse` (a clean up-front refusal) rather than an opaque QEMU
/// disk-lock error at spawn, proving the lock serialized the critical
/// section. `sample_config()` uses a fixed disk path, so both instances
/// point at the same disk without any cross-reference to `cfg_a`.
#[tokio::test]
async fn concurrent_starts_of_same_disk_instances_serialize_one_wins() {
    let daemon = Daemon::new();

    let cfg_a = sample_config();
    let id_a = cfg_a.id;
    daemon.create_instance(cfg_a).await.unwrap();

    let cfg_b = sample_config();
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let (ra, rb) = tokio::join!(daemon.start_instance(id_a), daemon.start_instance(id_b),);

    let a_ok = ra.is_ok();
    let mut oks = 0;
    match ra {
        Ok(()) => oks += 1,
        Err(e) => assert!(
            matches!(e, DaemonError::DiskInUse { .. }),
            "the losing concurrent start must fail with a disk conflict, got {e:?}"
        ),
    }
    match rb {
        Ok(()) => oks += 1,
        Err(e) => assert!(
            matches!(e, DaemonError::DiskInUse { .. }),
            "the losing concurrent start must fail with a disk conflict, got {e:?}"
        ),
    }
    assert_eq!(
        oks, 1,
        "exactly one of two concurrent same-disk starts must win"
    );

    let winner = if a_ok { id_a } else { id_b };
    let _ = daemon.stop_instance(winner, false).await;
}
