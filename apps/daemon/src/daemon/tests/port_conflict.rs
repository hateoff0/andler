use super::common::*;
use super::*;

#[tokio::test]
async fn start_rejects_host_port_forwarded_by_another_running_instance() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Running, None).await;

    let mut cfg_b = sample_config();
    cfg_b.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    let err = daemon
        .start_instance(id_b)
        .await
        .expect_err("start with a conflicting host port must fail");
    assert!(
        matches!(
            err,
            DaemonError::PortForwardConflict {
                port: 2222,
                instance: _,
                held_by,
            } if held_by == id_a
        ),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn start_allows_same_port_when_other_instance_is_stopped() {
    let daemon = Daemon::new();

    let mut cfg_a = sample_config();
    cfg_a.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_a = cfg_a.id;
    register_with_state(&daemon, cfg_a, InstanceState::Stopped, None).await;

    let mut cfg_b = sample_config();
    cfg_b.network.port_forwards = vec![andler_core::PortForward {
        protocol: andler_core::PortForwardProtocol::Tcp,
        host_port: 2222,
        guest_port: 22,
        host_address: None,
    }];
    let id_b = cfg_b.id;
    daemon.create_instance(cfg_b).await.unwrap();

    // No running instance holds the port, so the check passes and start
    // proceeds (a real qemu is spawned in this environment); stop it again
    // so no VM outlives the test.
    daemon
        .start_instance(id_b)
        .await
        .expect("start without a port conflict must proceed");
    daemon
        .stop_instance(id_b, false)
        .await
        .expect("stop the test VM again");
}
