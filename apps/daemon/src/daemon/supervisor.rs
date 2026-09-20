use std::future::Future;
use std::path::PathBuf;
use std::time::SystemTime;

use andler_core::{
    BackendHandle, DaemonEvent, EventKind, FsmError, GuestReadinessLevel, InstanceConfig,
    InstanceEvent, InstanceId, InstanceState, OpId, Operation, ProbeOutcome, ReadinessLadder,
    ReadinessProfile, ReadinessSnapshot, Resolution,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::error::DaemonError;
use super::ops::OpProgress;

/// One task per instance owns the instance's FSM state, backend handle and
/// config; everything else in the daemon reads those through watch snapshots
/// and mutates them through commands. This removes the structural RwLock
/// contention of the old shared map and gives the daemon a single place to
/// grow op-queueing and event sourcing onto.
#[derive(Clone)]
pub(crate) struct SupervisorHandle {
    pub(crate) id: InstanceId,
    pub(crate) instance_dir: std::path::PathBuf,
    pub(crate) cmd_tx: mpsc::Sender<SupervisorCommand>,
    pub(crate) state_rx: watch::Receiver<InstanceState>,
    pub(crate) handle_rx: watch::Receiver<Option<BackendHandle>>,
    pub(crate) config_rx: watch::Receiver<InstanceConfig>,
    /// Guest readiness ladder position of the current run: the level reached
    /// so far plus the profile the run's terminal level comes from.
    pub(crate) readiness_rx: watch::Receiver<ReadinessSnapshot>,
}

impl SupervisorHandle {
    pub(crate) fn instance_dir(&self) -> &std::path::Path {
        &self.instance_dir
    }
}

// commands carry payloads of very different sizes (an event with a message
// vs. the empty Shutdown); they are sent by reference through an mpsc
// channel, so the enum size is irrelevant
#[allow(clippy::large_enum_variant)]
pub(crate) enum SupervisorCommand {
    Transition {
        event: InstanceEvent,
        ack: oneshot::Sender<Result<InstanceState, FsmError>>,
    },
    SetHandle {
        handle: Option<BackendHandle>,
        ack: oneshot::Sender<Result<(), ()>>,
    },
    SetConfig {
        config: InstanceConfig,
        applied_live_resolution: Option<Resolution>,
        ack: oneshot::Sender<Result<(), ()>>,
    },
    /// Re-reads instance.toml when its mtime changed and reports the current
    /// file-vs-memory picture (for `config status` and as the read side of
    /// read-modify-write `config set`).
    ReloadConfig {
        ack: oneshot::Sender<Result<ConfigSnapshot, DaemonError>>,
    },
    /// Runs a long operation as a spawned sub-task (never inline — the first
    /// long-running command must not freeze status/cancel behind it). One
    /// active operation per instance; a second one either joins (same
    /// idempotency key) or is refused.
    RunOperation {
        op: Operation,
        key: Option<String>,
        run: OpRunner,
        done_tx: oneshot::Sender<Result<(), DaemonError>>,
        ack: oneshot::Sender<Result<Option<OpId>, DaemonError>>,
    },
    /// Same accept rule as `RunOperation`, for an operation that reports the
    /// id of the instance it creates (a clone). A same-key joiner learns that
    /// id from the in-flight operation, so a retried request gets the same
    /// answer as the request it retried.
    RunIdOperation {
        op: Operation,
        key: Option<String>,
        new_id: InstanceId,
        run: IdOpRunner,
        done_tx: oneshot::Sender<Result<InstanceId, DaemonError>>,
        ack: oneshot::Sender<Result<Option<(OpId, InstanceId)>, DaemonError>>,
    },
    CancelOperation {
        op_id: OpId,
        ack: oneshot::Sender<Result<bool, DaemonError>>,
    },
    /// Applies one readiness probe answer to the run's ladder. Answering with
    /// the level when the ladder advanced is the supervisor's job alone — it
    /// is the only writer, so the ladder cannot be advanced from two places
    /// at once.
    ObserveReadiness {
        level: GuestReadinessLevel,
        outcome: ProbeOutcome,
        ack: oneshot::Sender<ReadinessSnapshot>,
    },
    GetActiveOp {
        ack: oneshot::Sender<Option<Operation>>,
    },
    /// Answers the accept rule for a request that cannot start its own
    /// operation: the instance is mid-transition, and the caller must
    /// follow the same idempotency discipline `RunOperation` applies.
    JoinActive {
        key: Option<String>,
        ack: oneshot::Sender<Result<Option<OpId>, DaemonError>>,
    },
    Shutdown,
}

/// Outcome of enqueueing an operation.
#[derive(Debug)]
pub(crate) enum OpAccept {
    /// Fresh operation; the daemon waits on the paired `done_tx` for the
    /// final result.
    Started {
        done: oneshot::Receiver<Result<(), DaemonError>>,
    },
    /// An operation with the same idempotency key is already active; the
    /// caller joins it (progress is visible on the event bus).
    Joined { op_id: OpId },
}

/// Outcome of enqueueing an operation that creates an instance.
#[derive(Debug)]
pub(crate) enum IdOpAccept {
    /// Fresh operation; the daemon waits on the paired `done_tx` for the
    /// id of the instance it created.
    Started {
        done: oneshot::Receiver<Result<InstanceId, DaemonError>>,
    },
    /// A same-key clone is already in flight; the joiner learns the id that
    /// operation is creating (progress is visible on the event bus).
    Joined { op_id: OpId, new_id: InstanceId },
}

/// The supervisor's own accept rule for a request carrying an idempotency
/// key. `RunOperation` and `JoinActive` share it so the two paths can
/// never disagree about which request joins and which is refused.
#[derive(Debug)]
enum JoinDecision {
    /// An operation with this key is in flight and will do the work. A
    /// clone reports the instance id it is creating, so a retried request
    /// gets the same answer as the request it retried.
    Joined {
        active: Operation,
        new_id: Option<InstanceId>,
    },
    /// A different operation is in flight.
    Busy { active: Operation },
    /// Nothing in flight — or the in-flight operation was cancelled and
    /// will never complete its work, so a retry must start fresh.
    Free,
}

/// Long-running operation body: owns its progress handle, checks
/// `is_cancelled()` cooperatively between steps.
pub(crate) type OpRunner = Box<
    dyn FnOnce(
            OpProgress,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), DaemonError>> + Send>>
        + Send,
>;

/// Operation body that reports the id of the instance it created.
pub(crate) type IdOpRunner = Box<
    dyn FnOnce(
            OpProgress,
        )
            -> std::pin::Pin<Box<dyn Future<Output = Result<InstanceId, DaemonError>> + Send>>
        + Send,
>;

/// File-vs-memory config picture, produced by `ReloadConfig`.
#[derive(Debug, Clone)]
pub(crate) struct ConfigSnapshot {
    /// In-memory (live) config — what the daemon currently operates on.
    pub(crate) config: InstanceConfig,
    /// Config as last read from instance.toml, when the file is readable.
    /// Diffed against `config` to show pending manual edits on a
    /// Running/Paused instance, whose memory config is never silently
    /// overwritten by a file edit.
    pub(crate) file_config: Option<InstanceConfig>,
    /// Set when instance.toml exists but cannot be parsed or belongs to a
    /// different instance; the in-memory config is then left untouched.
    pub(crate) file_error: Option<String>,
    /// Resolution actually applied to the guest through the live RPC path,
    /// if any (may differ from both file and memory values).
    pub(crate) live_resolution: Option<Resolution>,
}

impl SupervisorHandle {
    pub(crate) fn state(&self) -> InstanceState {
        self.state_rx.borrow().clone()
    }

    pub(crate) fn backend_handle(&self) -> Option<BackendHandle> {
        self.handle_rx.borrow().clone()
    }

    pub(crate) fn config(&self) -> InstanceConfig {
        self.config_rx.borrow().clone()
    }

    /// Readiness ladder position of the current run (never a probe: the
    /// daemon's probe pass is what moves it).
    pub(crate) fn readiness(&self) -> ReadinessSnapshot {
        *self.readiness_rx.borrow()
    }

    /// Applies one probe answer to the ladder and reports the position the
    /// run is at afterwards.
    pub(crate) async fn observe_readiness(
        &self,
        level: GuestReadinessLevel,
        outcome: ProbeOutcome,
    ) -> Result<ReadinessSnapshot, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::ObserveReadiness {
                level,
                outcome,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))
    }

    /// Applies an FSM event inside the supervisor task. Errors are returned
    /// to the caller; the supervisor itself never blocks on persistence.
    pub(crate) async fn transition(
        &self,
        event: InstanceEvent,
    ) -> Result<InstanceState, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::Transition { event, ack: ack_tx })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        let result = ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        result.map_err(DaemonError::from)
    }

    pub(crate) async fn set_handle(
        &self,
        handle: Option<BackendHandle>,
    ) -> Result<(), DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::SetHandle {
                handle,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        let result = ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        result.map_err(|_| DaemonError::InstanceSupervisorGone(self.id))
    }

    pub(crate) async fn set_config(
        &self,
        config: InstanceConfig,
        applied_live_resolution: Option<Resolution>,
    ) -> Result<(), DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::SetConfig {
                config,
                applied_live_resolution,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        let result = ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        result.map_err(|_| DaemonError::InstanceSupervisorGone(self.id))
    }

    pub(crate) async fn reload_config(&self) -> Result<ConfigSnapshot, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::ReloadConfig { ack: ack_tx })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?
    }

    pub(crate) async fn run_operation(
        &self,
        op: Operation,
        key: Option<String>,
        run: OpRunner,
    ) -> Result<OpAccept, DaemonError> {
        let (done_tx, done_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::RunOperation {
                op,
                key,
                run,
                done_tx,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        match ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))??
        {
            // None = the operation was started fresh (this call owns done_rx);
            // Some(id) = joined an existing operation with the same key.
            None => Ok(OpAccept::Started { done: done_rx }),
            Some(op_id) => Ok(OpAccept::Joined { op_id }),
        }
    }

    pub(crate) async fn run_id_operation(
        &self,
        op: Operation,
        key: Option<String>,
        new_id: InstanceId,
        run: IdOpRunner,
    ) -> Result<IdOpAccept, DaemonError> {
        let (done_tx, done_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::RunIdOperation {
                op,
                key,
                new_id,
                run,
                done_tx,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        match ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))??
        {
            None => Ok(IdOpAccept::Started { done: done_rx }),
            Some((op_id, new_id)) => Ok(IdOpAccept::Joined { op_id, new_id }),
        }
    }

    pub(crate) async fn cancel_operation(&self, op_id: &str) -> Result<bool, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::CancelOperation {
                op_id: op_id.to_string(),
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?
    }

    pub(crate) async fn active_operation(&self) -> Result<Option<Operation>, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::GetActiveOp { ack: ack_tx })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))
    }

    pub(crate) async fn join_active(
        &self,
        key: Option<String>,
    ) -> Result<Option<OpId>, DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::JoinActive { key, ack: ack_tx })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?
    }

    pub(crate) async fn shutdown(&self) {
        let _ = self.cmd_tx.send(SupervisorCommand::Shutdown).await;
    }
}

struct InstanceSupervisor {
    id: InstanceId,
    state: InstanceState,
    handle: Option<BackendHandle>,
    config: InstanceConfig,
    events: broadcast::Sender<DaemonEvent>,
    state_tx: watch::Sender<InstanceState>,
    handle_tx: watch::Sender<Option<BackendHandle>>,
    config_tx: watch::Sender<InstanceConfig>,
    inbox: mpsc::Receiver<SupervisorCommand>,
    config_path: PathBuf,
    audit_dir: PathBuf,
    config_mtime: Option<SystemTime>,
    file_config: Option<InstanceConfig>,
    file_error: Option<String>,
    live_resolution: Option<Resolution>,
    /// Position on the guest readiness ladder for the current run. Reset
    /// whenever a run starts or ends, and re-derived whenever the effective
    /// `(kind, boot_mode)` profile changes (an Android boot-mode switch).
    ladder: ReadinessLadder,
    readiness_tx: watch::Sender<ReadinessSnapshot>,
    active_op: Option<watch::Sender<Operation>>,
    active_op_key: Option<String>,
    /// The instance id an in-flight clone is creating; reported to a
    /// same-key joiner so a retried clone reports the instance the first
    /// request created, not one it invented itself.
    active_op_new_id: Option<InstanceId>,
    /// True while the active operation has been cancelled but is still
    /// unwinding (e.g. blocked on a gate). A same-key retry must start
    /// fresh here, not join an op that will never complete its work.
    active_op_cancelled: bool,
    /// Generation token of the operation in `active_op`. A superseded
    /// (cancelled) op keeps running until it unwinds; its completion must
    /// not clear the operation that replaced it.
    active_op_seq: Option<u64>,
    next_op_seq: u64,
    op_cancel: Option<watch::Sender<bool>>,
    op_done_tx: mpsc::UnboundedSender<u64>,
    op_done_rx: mpsc::UnboundedReceiver<u64>,
}

pub(crate) fn spawn_supervisor(
    id: InstanceId,
    instance_dir: std::path::PathBuf,
    config: InstanceConfig,
    state: InstanceState,
    handle: Option<BackendHandle>,
    events: broadcast::Sender<DaemonEvent>,
) -> SupervisorHandle {
    let (cmd_tx, inbox) = mpsc::channel(64);
    let (state_tx, state_rx) = watch::channel(state.clone());
    let (handle_tx, handle_rx) = watch::channel(handle.clone());
    let (config_tx, config_rx) = watch::channel(config.clone());
    let (op_done_tx, op_done_rx) = mpsc::unbounded_channel::<u64>();
    let ladder = ReadinessLadder::new(ReadinessProfile::of(&config.kind));
    let (readiness_tx, readiness_rx) = watch::channel(ladder.snapshot());

    // The registry directory (instances_root/<id>) owns instance.toml and
    // events.jsonl. It is NOT derived from disk.path — a config can point
    // its disk anywhere (e.g. `create --file` fixtures), so the disk's
    // parent directory is not the instance's registry directory.
    let config_path = instance_dir.join("instance.toml");
    let audit_dir = instance_dir.clone();
    let supervisor = InstanceSupervisor {
        id,
        state,
        handle,
        config,
        events,
        state_tx,
        handle_tx,
        config_tx,
        inbox,
        config_path,
        audit_dir,
        config_mtime: None,
        file_config: None,
        file_error: None,
        live_resolution: None,
        ladder,
        readiness_tx,
        active_op: None,
        active_op_key: None,
        active_op_new_id: None,
        active_op_cancelled: false,
        active_op_seq: None,
        next_op_seq: 0,
        op_cancel: None,
        op_done_tx,
        op_done_rx,
    };
    // Watchdog: never fire-and-forget a task that owns instance
    // state. The JoinHandle is awaited by a tiny watcher so a supervisor
    // panic is logged with the instance id instead of surfacing only as a
    // silently closed command channel.
    let join = tokio::spawn(async move {
        supervisor.run().await;
    });
    tokio::spawn(async move {
        if let Err(error) = join.await {
            tracing::error!(instance_id = %id, %error, "instance supervisor task failed");
        }
    });

    SupervisorHandle {
        id,
        instance_dir,
        cmd_tx,
        state_rx,
        handle_rx,
        config_rx,
        readiness_rx,
    }
}

impl InstanceSupervisor {
    // Intentionally sequential: every command today (FSM transition, handle/
    // config swap) completes in milliseconds. Long-running commands
    // (DiskChain ops, provisioning) MUST spawn their own task and ack
    // through their handle — awaiting them inline freezes status/cancel
    // behind them. RunOperation below is that path.
    async fn run(mut self) {
        loop {
            tokio::select! {
                done = self.op_done_rx.recv() => {
                    match done {
                        Some(seq) => {
                            // A superseded op unwinds after its replacement
                            // was already accepted; its completion must not
                            // clear the operation that took its place.
                            if self.active_op_seq == Some(seq) {
                                self.active_op = None;
                                self.active_op_key = None;
                                self.active_op_new_id = None;
                                self.active_op_seq = None;
                                self.active_op_cancelled = false;
                                self.op_cancel = None;
                            }
                        }
                        None => break,
                    }
                }
                command = self.inbox.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        SupervisorCommand::Transition { event, ack } => {
                            self.apply_transition(event, ack).await;
                        }
                        SupervisorCommand::SetHandle { handle, ack } => {
                            self.handle = handle.clone();
                            let _ = self.handle_tx.send(handle);
                            let _ = ack.send(Ok(()));
                        }
                        SupervisorCommand::SetConfig {
                            config,
                            applied_live_resolution,
                            ack,
                        } => {
                            if let Some(resolution) = applied_live_resolution {
                                self.live_resolution = Some(resolution);
                            }
                            self.config = config.clone();
                            self.rebase_ladder();
                            let _ = self.config_tx.send(config);
                            self.persist().await;
                            let _ = ack.send(Ok(()));
                        }
                        SupervisorCommand::ReloadConfig { ack } => {
                            self.reload_if_changed().await;
                            let _ = ack.send(Ok(ConfigSnapshot {
                                config: self.config.clone(),
                                file_config: self.file_config.clone(),
                                file_error: self.file_error.clone(),
                                live_resolution: self.live_resolution,
                            }));
                        }
                        SupervisorCommand::RunOperation {
                            op,
                            key,
                            run,
                            done_tx,
                            ack,
                        } => {
                            self.start_operation(op, key, run, done_tx, ack).await;
                        }
                        SupervisorCommand::RunIdOperation {
                            op,
                            key,
                            new_id,
                            run,
                            done_tx,
                            ack,
                        } => {
                            self.start_id_operation(op, key, new_id, run, done_tx, ack)
                                .await;
                        }
                        SupervisorCommand::CancelOperation { op_id, ack } => {
                            let is_active = self
                                .active_op
                                .as_ref()
                                .is_some_and(|active| active.borrow().op_id == op_id);
                            let cancelled = match &self.op_cancel {
                                Some(cancel_tx) if is_active => {
                                    let _ = cancel_tx.send(true);
                                    self.active_op_cancelled = true;
                                    true
                                }
                                _ => false,
                            };
                            let _ = ack.send(Ok(cancelled));
                        }
                        SupervisorCommand::GetActiveOp { ack } => {
                            let active = self
                                .active_op
                                .as_ref()
                                .map(|active| active.borrow().clone());
                            let _ = ack.send(active);
                        }
                        SupervisorCommand::ObserveReadiness {
                            level,
                            outcome,
                            ack,
                        } => {
                            let _ = ack.send(self.observe_readiness(level, outcome).await);
                        }
                        SupervisorCommand::JoinActive { key, ack } => {
                            let _ = ack.send(match self.join_decision(key.as_ref()) {
                                JoinDecision::Joined { active, .. } => {
                                    Ok(Some(active.op_id))
                                }
                                JoinDecision::Busy { active } => {
                                    Err(self.already_running(&active))
                                }
                                JoinDecision::Free => Ok(None),
                            });
                        }
                        SupervisorCommand::Shutdown => break,
                    }
                }
            }
        }
        tracing::debug!(instance_id = %self.id, "instance supervisor stopped");
    }

    /// Enqueues a long-running operation and runs it as a spawned sub-task:
    /// one active operation per instance; a second one either joins (same
    /// idempotency key) or is refused with an actionable error. The ack is
    /// sent as soon as the operation is accepted — the caller then waits on
    /// `done_tx` — so status/cancel/transition commands never queue behind
    /// the operation's own execution.
    async fn start_operation(
        &mut self,
        op: Operation,
        key: Option<String>,
        run: OpRunner,
        done_tx: oneshot::Sender<Result<(), DaemonError>>,
        ack: oneshot::Sender<Result<Option<OpId>, DaemonError>>,
    ) {
        match self.join_decision(key.as_ref()) {
            JoinDecision::Joined { active, .. } => {
                let _ = ack.send(Ok(Some(active.op_id)));
                return;
            }
            JoinDecision::Busy { active } => {
                let _ = ack.send(Err(self.already_running(&active)));
                return;
            }
            // Nothing in flight, or a cancelled op that will never complete
            // its work: start fresh and overwrite any superseded
            // bookkeeping (the generation guard ignores the late ack).
            JoinDecision::Free => {}
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let events = self.events.clone();
        let audit_dir = self.audit_dir.clone();
        let op_done_tx = self.op_done_tx.clone();
        let seq = self.next_op_seq;
        self.next_op_seq += 1;
        let phases = op.phases.clone();
        let op_tx = watch::Sender::new(op);
        self.active_op = Some(op_tx.clone());
        self.active_op_key = key;
        self.active_op_new_id = None;
        self.active_op_seq = Some(seq);
        self.active_op_cancelled = false;
        self.op_cancel = Some(cancel_tx);

        tokio::spawn(async move {
            let mut progress = OpProgress::new(op_tx, phases, events, audit_dir, cancel_rx);
            progress.mark_running();
            // The operation itself calls progress.finish() (it owns the
            // handle); the sub-task only forwards the result.
            let result = run(progress).await;
            let _ = done_tx.send(result);
            let _ = op_done_tx.send(seq);
        });
        let _ = ack.send(Ok(None));
    }

    /// Clone-shaped `start_operation`: same accept rule, but the body
    /// reports the id of the instance it created and a same-key joiner
    /// learns it from `active_op_new_id`.
    async fn start_id_operation(
        &mut self,
        op: Operation,
        key: Option<String>,
        new_id: InstanceId,
        run: IdOpRunner,
        done_tx: oneshot::Sender<Result<InstanceId, DaemonError>>,
        ack: oneshot::Sender<Result<Option<(OpId, InstanceId)>, DaemonError>>,
    ) {
        match self.join_decision(key.as_ref()) {
            JoinDecision::Joined {
                active,
                new_id: Some(new_id),
            } => {
                let _ = ack.send(Ok(Some((active.op_id, new_id))));
                return;
            }
            // A token held by an operation that creates no instance (a
            // client reusing a token across different operations) cannot
            // report an id: refuse rather than invent one.
            JoinDecision::Joined { active, .. } | JoinDecision::Busy { active } => {
                let _ = ack.send(Err(self.already_running(&active)));
                return;
            }
            JoinDecision::Free => {}
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let events = self.events.clone();
        let audit_dir = self.audit_dir.clone();
        let op_done_tx = self.op_done_tx.clone();
        let seq = self.next_op_seq;
        self.next_op_seq += 1;
        let phases = op.phases.clone();
        let op_tx = watch::Sender::new(op);
        self.active_op = Some(op_tx.clone());
        self.active_op_key = key;
        self.active_op_new_id = Some(new_id);
        self.active_op_seq = Some(seq);
        self.active_op_cancelled = false;
        self.op_cancel = Some(cancel_tx);

        tokio::spawn(async move {
            let mut progress = OpProgress::new(op_tx, phases, events, audit_dir, cancel_rx);
            progress.mark_running();
            let result = run(progress).await;
            let _ = done_tx.send(result);
            let _ = op_done_tx.send(seq);
        });
        let _ = ack.send(Ok(None));
    }

    fn join_decision(&self, key: Option<&String>) -> JoinDecision {
        match &self.active_op {
            Some(active) => {
                let active = active.borrow().clone();
                if key.is_some() && key == self.active_op_key.as_ref() {
                    if self.active_op_cancelled {
                        JoinDecision::Free
                    } else {
                        JoinDecision::Joined {
                            active,
                            new_id: self.active_op_new_id,
                        }
                    }
                } else {
                    JoinDecision::Busy { active }
                }
            }
            None => JoinDecision::Free,
        }
    }

    fn already_running(&self, active: &Operation) -> DaemonError {
        DaemonError::OperationAlreadyRunning {
            instance_id: self.id,
            active_op_id: active.op_id.clone(),
            active_kind: active.kind,
        }
    }

    /// Applies one probe answer. The ladder advances at most once per level
    /// and never moves down; an advance is published on the daemon bus and
    /// appended to the audit trail, so consumers read levels from the bus
    /// instead of deriving them.
    async fn observe_readiness(
        &mut self,
        level: GuestReadinessLevel,
        outcome: ProbeOutcome,
    ) -> ReadinessSnapshot {
        if let Some(reached) = self.ladder.observe(level, outcome) {
            let event = DaemonEvent {
                ts_ms: chrono::Utc::now().timestamp_millis() as u64,
                instance_id: Some(self.id),
                kind: EventKind::Readiness { level: reached },
            };
            tracing::info!(
                instance_id = %self.id,
                level = ?reached,
                "guest readiness level reached"
            );
            let _ = self.events.send(event.clone());
            super::audit::append_event(&self.audit_dir, &event).await;
        }
        let snapshot = self.ladder.snapshot();
        self.readiness_tx.send_replace(snapshot);
        snapshot
    }

    /// Clears the ladder and republishes: the position is per run, so a run
    /// that has not started (or has ended) reports no level.
    fn reset_ladder(&mut self) {
        self.ladder = ReadinessLadder::new(ReadinessProfile::of(&self.config.kind));
        self.readiness_tx.send_replace(self.ladder.snapshot());
    }

    /// Re-derives the ladder when the effective `(kind, boot_mode)` profile
    /// changed (an Android boot-mode switch). A run of a different profile is
    /// a different ladder, not a continuation of the old one.
    fn rebase_ladder(&mut self) {
        if self.ladder.snapshot().profile != ReadinessProfile::of(&self.config.kind) {
            self.reset_ladder();
        }
    }

    async fn apply_transition(
        &mut self,
        event: InstanceEvent,
        ack: oneshot::Sender<Result<InstanceState, FsmError>>,
    ) {
        let from = self.state.clone();
        match self.state.clone().apply(event.clone()) {
            Ok(new_state) => {
                self.state = new_state.clone();
                // A run's ladder belongs to that run: a new run starts empty,
                // and a run that is no longer live keeps no level to report.
                if matches!(
                    new_state,
                    InstanceState::Starting
                        | InstanceState::Stopping
                        | InstanceState::Stopped
                        | InstanceState::Error { .. }
                ) {
                    self.reset_ladder();
                }
                let reason = match &event {
                    InstanceEvent::Fail(message) => Some(message.clone()),
                    _ => None,
                };
                let event = DaemonEvent {
                    ts_ms: chrono::Utc::now().timestamp_millis() as u64,
                    instance_id: Some(self.id),
                    kind: EventKind::Lifecycle {
                        from,
                        to: new_state.clone(),
                        reason,
                    },
                };
                self.persist().await;
                let _ = self.state_tx.send(new_state.clone());
                let _ = self.events.send(event.clone());
                super::audit::append_event(&self.audit_dir, &event).await;
                let _ = ack.send(Ok(new_state));
            }
            Err(err) => {
                let _ = ack.send(Err(err));
            }
        }
    }

    /// Re-reads instance.toml when its mtime changed. On an idle instance
    /// (Created/Stopped/Error) the file is the source of truth: manual edits
    /// are picked up here and pushed through config_tx. On a Running/Paused
    /// instance the memory config is the live one — a file edit is never
    /// applied silently, it is only cached as `file_config` so `config
    /// status` can report it as pending (it applies at the next stop/start
    /// or through `config set`). A file that cannot be parsed leaves the
    /// in-memory config untouched and records the reason.
    async fn reload_if_changed(&mut self) {
        let meta = match tokio::fs::metadata(&self.config_path).await {
            Ok(meta) => meta,
            Err(_) => {
                self.config_mtime = None;
                self.file_config = None;
                return;
            }
        };
        let mtime = meta.modified().ok();
        if self.config_mtime.is_some() && mtime == self.config_mtime {
            return;
        }

        let content = match tokio::fs::read_to_string(&self.config_path).await {
            Ok(content) => content,
            Err(err) => {
                self.file_error = Some(format!("cannot read instance.toml: {err}"));
                return;
            }
        };

        match andler_core::config::parse_instance_config_toml(&content) {
            Ok(cfg) if cfg.id == self.id => {
                self.file_config = Some(cfg.clone());
                self.config_mtime = mtime;
                self.file_error = None;
                if self.state.is_disk_idle() {
                    self.config = cfg.clone();
                    self.rebase_ladder();
                    let _ = self.config_tx.send(cfg);
                }
            }
            Ok(_) => {
                self.file_config = None;
                self.file_error = Some(
                    "instance.toml belongs to a different instance id; \
                     refusing to load it"
                        .to_string(),
                );
            }
            Err(err) => {
                self.file_config = None;
                self.file_error = Some(format!("invalid instance.toml: {err}"));
            }
        }
    }

    /// Writes the in-memory config to instance.toml (atomic tmp+rename),
    /// after picking up any manual edits via reload so they are not
    /// clobbered by the write. Refreshes the cached mtime so the write does
    /// not look like a manual change.
    async fn persist(&mut self) {
        self.reload_if_changed().await;
        super::types::write_instance_toml(&self.audit_dir, &self.config).await;
        if let Ok(meta) = tokio::fs::metadata(&self.config_path).await {
            self.config_mtime = meta.modified().ok();
        }
        // The file now holds exactly what memory holds, and the mtime guard
        // above would never re-read it: keep the cached snapshot honest.
        self.file_config = Some(self.config.clone());
        self.file_error = None;
    }
}
