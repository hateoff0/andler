use super::common::TestTempDir;
use super::supervisor::{OpAccept, OpRunner};
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
