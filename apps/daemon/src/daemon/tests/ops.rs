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
        current_phase: None,
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
        current_phase: None,
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

/// Like [`sample_supervisor`], but handing back the event sender so a test can
/// subscribe to the bus the way a joining caller does.
async fn sample_supervisor_with_events() -> (
    TestTempDir,
    SupervisorHandle,
    InstanceId,
    tokio::sync::broadcast::Sender<DaemonEvent>,
) {
    let dir = TestTempDir::new();
    let id = InstanceId::new();
    let cfg = super::common::sample_config();
    let (events, _) = tokio::sync::broadcast::channel(16);
    let handle = spawn_supervisor(
        id,
        dir.path().join(id.to_string()),
        cfg,
        InstanceState::Created,
        None,
        events.clone(),
    );
    (dir, handle, id, events)
}

/// A join must wait for the operation it joined. Returning as soon as the join
/// succeeded is how `guest install libndk` reported "installed" while the
/// staging batch was still running, and how a failed switch looked like a
/// successful one.
#[tokio::test]
async fn a_join_waits_for_the_operation_and_surfaces_its_outcome() {
    use crate::daemon::instance_ops::wait_for_joined_operation;

    let (_dir, handle, id, events) = sample_supervisor_with_events().await;
    let mut waiter_events = events.subscribe();

    // A failing operation: the joined caller must see the failure, not success.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let run: OpRunner = Box::new(move |mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            let _ = release_rx.await;
            progress.finish(Err("the appliance never answered".to_string()));
            Err(DaemonError::OperationFailed {
                op_id: "op-test".to_string(),
                reason: "the appliance never answered".to_string(),
            })
        })
    });
    let first = handle
        .run_operation(
            test_op(id, OperationKind::GuestInstall),
            Some("key".to_string()),
            run,
        )
        .await
        .expect("first operation must be accepted");
    assert!(matches!(first, OpAccept::Started { .. }));

    let waiter_handle = handle.clone();
    let waiter = tokio::spawn(async move {
        wait_for_joined_operation(&waiter_handle, &mut waiter_events, "op-test").await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !waiter.is_finished(),
        "a join must not report a result while the work is still running"
    );

    let _ = release_tx.send(());
    let result = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the waiter returns once the operation finishes")
        .expect("the waiting task must not panic");
    assert!(
        matches!(result, Err(DaemonError::OperationFailed { .. })),
        "a failed joined operation must surface as an error, got {result:?}"
    );
}

/// The phase the runner entered is the one the CLI must name. A translator
/// switch whose payload is already cached skips `downloading` and stages
/// straight away; the CLI used to name the phase back from the progress
/// weight, so it printed "downloading" for minutes while the daemon staged.
#[tokio::test]
async fn a_skipped_phase_is_never_reported_as_the_running_one() {
    let (_dir, handle, id, events) = sample_supervisor_with_events().await;
    let mut rx = events.subscribe();

    let run: OpRunner = Box::new(|mut progress| {
        Box::pin(async move {
            progress.mark_running();
            progress.enter_phase("staging");
            progress.finish(Ok(()));
            Ok(())
        })
    });

    let accept = handle
        .run_operation(
            Operation {
                op_id: "op-skipped-phase".to_string(),
                instance_id: id,
                kind: OperationKind::GuestInstall,
                phases: vec![
                    ("downloading".to_string(), 0.5),
                    ("staging".to_string(), 0.5),
                ],
                progress: 0.0,
                current_phase: None,
                state: OperationState::Queued,
                error: None,
            },
            None,
            run,
        )
        .await
        .expect("operation must be accepted");
    let OpAccept::Started { done } = accept else {
        unreachable!()
    };
    done.await.expect("finish").expect("Ok");

    let mut published = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let EventKind::Operation { op } = event.kind {
            published.push(op);
        }
    }

    let staged = published
        .iter()
        .find(|op| op.state == OperationState::Running && op.progress > 0.0)
        .expect("the running phase must be published");
    assert_eq!(
        staged.current_phase.as_deref(),
        Some("staging"),
        "the phase that ran must be reported, not the weight-earliest one"
    );
    // 0.5 is exactly the boundary the CLI used to resolve to "downloading".
    assert_eq!(staged.progress, 0.5);
    assert!(
        published
            .iter()
            .all(|op| op.current_phase.as_deref() != Some("downloading")),
        "a phase that never ran must not be reported"
    );
}

/// The registry `list_operations` answers from is what `andler op list` and
/// every progress line poll while an operation runs. It must report the phase
/// and progress the runner is at *now*: a copy taken when the operation was
/// accepted leaves the CLI frozen on the first phase for the whole operation,
/// however many phases the runner enters.
#[tokio::test]
async fn active_operation_tracks_the_phase_the_runner_entered() {
    let (_dir, handle, id) = sample_supervisor().await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let run: OpRunner = Box::new(move |mut progress| {
        Box::pin(async move {
            progress.enter_phase("phase-a");
            // Both mutations land before `entered_tx` fires, so the assertion
            // below never races the runner.
            progress.enter_phase("phase-b");
            progress.set_progress(0.5);
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            progress.finish(Ok(()));
            Ok(())
        })
    });

    let accept = handle
        .run_operation(test_op(id, OperationKind::GuestInstall), None, run)
        .await
        .expect("operation must be accepted");
    let OpAccept::Started { done } = accept else {
        unreachable!()
    };
    entered_rx.await.expect("the runner must reach phase-b");

    let active = handle
        .active_operation()
        .await
        .expect("the supervisor must answer")
        .expect("the operation is still running");
    assert_eq!(
        active.current_phase.as_deref(),
        Some("phase-b"),
        "the registry must name the phase the runner is in"
    );
    assert_eq!(
        active.progress, 0.75,
        "phase-b (weight 0.5) at half progress must be 0.75, not the phase it started in"
    );
    assert_eq!(active.state, OperationState::Running);

    let _ = release_tx.send(());
    done.await.expect("done must resolve").expect("Ok");
}
