use super::*;

#[tokio::test]
async fn lifecycle_events_are_broadcast_on_fsm_transitions() {
    let daemon = Daemon::new();
    let id = InstanceId::new();
    let cfg = common::sample_config();
    {
        let mut instances = daemon.instances.write().await;
        instances.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Created,
                handle: None,
            },
        );
    }

    let mut rx = daemon.subscribe_events();
    daemon
        .apply_event_and_persist(id, InstanceEvent::Start)
        .await;
    daemon
        .apply_event_and_persist(id, InstanceEvent::StartCompleted)
        .await;

    let first = rx.recv().await.expect("event 1");
    assert_eq!(first.instance_id, Some(id));
    assert!(
        matches!(
            first.kind,
            EventKind::Lifecycle {
                from: InstanceState::Created,
                to: InstanceState::Starting,
                reason: None
            }
        ),
        "unexpected first event: {first:?}"
    );

    let second = rx.recv().await.expect("event 2");
    assert!(
        matches!(
            second.kind,
            EventKind::Lifecycle {
                from: InstanceState::Starting,
                to: InstanceState::Running,
                reason: None
            }
        ),
        "unexpected second event: {second:?}"
    );
}

#[tokio::test]
async fn fail_transition_carries_the_reason_in_the_event() {
    let daemon = Daemon::new();
    let id = InstanceId::new();
    let cfg = common::sample_config();
    {
        let mut instances = daemon.instances.write().await;
        instances.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Running,
                handle: None,
            },
        );
    }

    let mut rx = daemon.subscribe_events();
    daemon
        .apply_event_and_persist(id, InstanceEvent::Fail("qemu crashed".into()))
        .await;

    let event = rx.recv().await.expect("event");
    match event.kind {
        EventKind::Lifecycle {
            from: InstanceState::Running,
            to: InstanceState::Error { .. },
            reason: Some(reason),
        } => assert_eq!(reason, "qemu crashed"),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn events_channel_drops_when_no_subscriber_listens() {
    let daemon = Daemon::new();
    let id = InstanceId::new();
    let cfg = common::sample_config();
    {
        let mut instances = daemon.instances.write().await;
        instances.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Created,
                handle: None,
            },
        );
    }

    // No receiver: emit must not panic and must not block.
    daemon
        .apply_event_and_persist(id, InstanceEvent::Start)
        .await;
}
