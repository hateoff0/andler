use super::common::TestTempDir;
use super::supervisor::{IdOpAccept, IdOpRunner, OpAccept, OpRunner};
use super::*;
use andler_core::{EventKind, InstanceEvent, Operation, OperationKind, OperationState};
use std::time::Duration;

async fn sample_supervisor() -> (TestTempDir, SupervisorHandle, InstanceId) {
    let dir = TestTempDir::new();
    let id = InstanceId::new();
    let cfg = common::sample_config();
    let (events, _) = tokio::sync::broadcast::channel(16);
    let handle = spawn_supervisor(
        id,
        dir.path().join(id.to_string()),
        cfg,
        InstanceState::Created,
        None,
        events,
    );
    (dir, handle, id)
}

fn test_op(id: InstanceId, kind: OperationKind) -> Operation {
    Operation {
        op_id: "op-test".to_string(),
        instance_id: id,
        kind,
        phases: vec![("phase-a".to_string(), 0.5), ("phase-b".to_string(), 0.5)],
        progress: 0.0,
        state: OperationState::Queued,
        error: None,
    }
}

#[tokio::test]
async fn long_operation_does_not_block_transitions() {
    let (_dir, handle, id) = sample_supervisor().await;

    let run: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            // A genuinely long operation: transition below must not wait
            // for this to finish.
            tokio::time::sleep(Duration::from_millis(400)).await;
            progress.finish(Ok(()));
            Ok(())
        })
    });

    let started = std::time::Instant::now();
    let accept = handle
        .run_operation(
            test_op(id, OperationKind::SnapshotRestore),
            Some("k1".to_string()),
            run,
        )
        .await
        .expect("operation must be accepted");
    assert!(matches!(accept, OpAccept::Started { .. }));

    // While the operation runs, a transition must be answered promptly.
    let transition = handle
        .transition(InstanceEvent::Start)
        .await
        .expect("transition must not queue behind the long operation");
    assert_eq!(transition, InstanceState::Starting);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "transition waited for the long operation: {:?}",
        started.elapsed()
    );

    let OpAccept::Started { done } = accept else {
        unreachable!()
    };
    done.await
        .expect("done channel must resolve")
        .expect("operation must finish Ok");
    assert_eq!(handle.state(), InstanceState::Starting);
}

#[tokio::test]
async fn cancel_operation_sets_token_and_marks_cancelled() {
    let (_dir, handle, id) = sample_supervisor().await;

    let run: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            // Poll the token instead of sleeping blind; cancel must stop us.
            for _ in 0..100 {
                if progress.is_cancelled() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(DaemonError::OperationCancelled("not cancelled".to_string()))
        })
    });

    let accept = handle
        .run_operation(test_op(id, OperationKind::SnapshotRestore), None, run)
        .await
        .expect("operation must be accepted");
    let OpAccept::Started { done } = accept else {
        unreachable!()
    };

    tokio::time::sleep(Duration::from_millis(30)).await;
    let cancelled = handle
        .cancel_operation("op-test")
        .await
        .expect("cancel must reach the supervisor");
    assert!(cancelled);

    let result = done
        .await
        .expect("done channel must resolve")
        .expect("cooperative cancel returns Ok, not an error");
    assert_eq!(result, ());
    assert!(handle.active_operation().await.unwrap().is_none());
}

#[tokio::test]
async fn same_key_joins_second_operation() {
    let (_dir, handle, id) = sample_supervisor().await;

    let run_a: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            tokio::time::sleep(Duration::from_millis(200)).await;
            progress.finish(Ok(()));
            Ok(())
        })
    });
    let first = handle
        .run_operation(
            test_op(id, OperationKind::SnapshotRestore),
            Some("key".to_string()),
            run_a,
        )
        .await
        .expect("first operation must be accepted");
    assert!(matches!(first, OpAccept::Started { .. }));

    let run_b: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            tokio::time::sleep(Duration::from_millis(200)).await;
            progress.finish(Ok(()));
            Ok(())
        })
    });
    let second = handle
        .run_operation(
            test_op(id, OperationKind::SnapshotRestore),
            Some("key".to_string()),
            run_b,
        )
        .await
        .expect("second operation with the same key must join, not error");
    match second {
        OpAccept::Joined { op_id } => assert_eq!(op_id, "op-test"),
        OpAccept::Started { .. } => panic!("expected Joined for a duplicate key"),
    }

    let run_c: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            tokio::time::sleep(Duration::from_millis(200)).await;
            progress.finish(Ok(()));
            Ok(())
        })
    });
    let different = handle
        .run_operation(
            test_op(id, OperationKind::SnapshotDelete),
            Some("other".to_string()),
            run_c,
        )
        .await
        .expect_err("a different operation must be refused while one is active");
    assert!(matches!(
        different,
        DaemonError::OperationAlreadyRunning { .. }
    ));

    let OpAccept::Started { done } = first else {
        unreachable!()
    };
    done.await
        .expect("first operation must finish")
        .expect("Ok");
}

fn test_op_with(id: InstanceId, kind: OperationKind, op_id: &str) -> Operation {
    Operation {
        op_id: op_id.to_string(),
        instance_id: id,
        kind,
        phases: vec![("phase-a".to_string(), 1.0)],
        progress: 0.0,
        state: OperationState::Queued,
        error: None,
    }
}

fn blocked_id_run(new_id: andler_core::InstanceId) -> IdOpRunner {
    Box::new(move |mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            tokio::time::sleep(Duration::from_millis(200)).await;
            progress.finish(Ok(()));
            Ok(new_id)
        })
    })
}

/// The contract a retried clone relies on: a same-key join learns the id the
/// running operation is creating, never the id the joiner invented for
/// itself, and a different key is still refused.
#[tokio::test]
async fn clone_join_reports_the_instance_the_running_operation_creates() {
    let (_dir, handle, id) = sample_supervisor().await;
    let first_id = InstanceId::new();
    let joiner_id = InstanceId::new();

    let first = handle
        .run_id_operation(
            test_op_with(id, OperationKind::Clone, "clone-1"),
            Some("clone-key".to_string()),
            first_id,
            blocked_id_run(first_id),
        )
        .await
        .expect("first clone must be accepted");
    assert!(matches!(first, IdOpAccept::Started { .. }));

    let second = handle
        .run_id_operation(
            test_op_with(id, OperationKind::Clone, "clone-2"),
            Some("clone-key".to_string()),
            joiner_id,
            blocked_id_run(joiner_id),
        )
        .await
        .expect("a same-key clone retry must join");
    match second {
        IdOpAccept::Joined { op_id, new_id } => {
            assert_eq!(op_id, "clone-1", "the joiner names the running op");
            assert_eq!(
                new_id, first_id,
                "the joiner must learn the running op's id, not its own"
            );
            assert_ne!(new_id, joiner_id, "the joiner's own id must be dropped");
        }
        IdOpAccept::Started { .. } => panic!("expected Joined for a duplicate key"),
    }

    let different = handle
        .run_id_operation(
            test_op_with(id, OperationKind::Clone, "clone-3"),
            Some("other-key".to_string()),
            InstanceId::new(),
            blocked_id_run(InstanceId::new()),
        )
        .await
        .expect_err("a different key must be refused while a clone is active");
    assert!(matches!(
        different,
        DaemonError::OperationAlreadyRunning { .. }
    ));

    let IdOpAccept::Started { done } = first else {
        unreachable!()
    };
    done.await
        .expect("done channel must resolve")
        .expect("the running clone creates its instance");
}

/// A token reused across operations that do not create an instance cannot
/// report an id: the reply must be a refusal, never an invented instance.
#[tokio::test]
async fn clone_join_over_a_non_clone_operation_is_refused() {
    let (_dir, handle, id) = sample_supervisor().await;

    let run: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            tokio::time::sleep(Duration::from_millis(200)).await;
            progress.finish(Ok(()));
            Ok(())
        })
    });
    let first = handle
        .run_operation(
            test_op_with(id, OperationKind::SnapshotDelete, "delete-1"),
            Some("shared".to_string()),
            run,
        )
        .await
        .expect("the unit-shaped operation must be accepted");
    assert!(matches!(first, OpAccept::Started { .. }));

    let err = handle
        .run_id_operation(
            test_op_with(id, OperationKind::Clone, "clone-1"),
            Some("shared".to_string()),
            InstanceId::new(),
            blocked_id_run(InstanceId::new()),
        )
        .await
        .expect_err("a clone cannot join an operation that creates no instance");
    assert!(
        matches!(err, DaemonError::OperationAlreadyRunning { active_kind: OperationKind::SnapshotDelete, .. } if true),
        "must name the running operation instead of inventing an id, got: {err:?}"
    );

    let OpAccept::Started { done } = first else {
        unreachable!()
    };
    done.await.expect("resolve").expect("Ok");

    // Once the running operation is gone the same key starts fresh again.
    let fresh = handle
        .run_id_operation(
            test_op_with(id, OperationKind::Clone, "clone-2"),
            Some("shared".to_string()),
            InstanceId::new(),
            blocked_id_run(InstanceId::new()),
        )
        .await
        .expect("the key must be free after the operation finished");
    assert!(matches!(fresh, IdOpAccept::Started { .. }));
    let IdOpAccept::Started { done } = fresh else {
        unreachable!()
    };
    done.await.expect("resolve").expect("Ok");
}

#[tokio::test]
async fn operation_events_reach_the_bus() {
    let dir = TestTempDir::new();
    let id = InstanceId::new();
    let cfg = common::sample_config();
    let (events, mut rx) = tokio::sync::broadcast::channel(16);
    let handle = spawn_supervisor(
        id,
        dir.path().join(id.to_string()),
        cfg,
        InstanceState::Created,
        None,
        events,
    );

    let run: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.mark_running();
            progress.enter_phase("phase-a");
            progress.finish(Ok(()));
            Ok(())
        })
    });

    let accept = handle
        .run_operation(test_op(id, OperationKind::SnapshotRestore), None, run)
        .await
        .expect("operation must be accepted");
    let OpAccept::Started { done } = accept else {
        unreachable!()
    };
    done.await.expect("finish").expect("Ok");

    let mut saw_done = false;
    for _ in 0..10 {
        match rx.recv().await {
            Ok(event) => {
                if let EventKind::Operation { op } = event.kind {
                    if op.state == OperationState::Done {
                        saw_done = true;
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    assert!(saw_done, "expected a Done operation event on the bus");
}
