use std::path::PathBuf;
use std::time::SystemTime;

use andler_core::{
    BackendHandle, DaemonEvent, EventKind, FsmError, InstanceConfig, InstanceEvent, InstanceId,
    InstanceState, OpId, Operation, Resolution,
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
    CancelOperation {
        op_id: OpId,
        ack: oneshot::Sender<Result<bool, DaemonError>>,
    },
    GetActiveOp {
        ack: oneshot::Sender<Option<Operation>>,
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

/// Long-running operation body: owns its progress handle, checks
/// `is_cancelled()` cooperatively between steps.
pub(crate) type OpRunner = Box<
    dyn FnOnce(
            OpProgress,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), DaemonError>> + Send>,
        > + Send,
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
    active_op: Option<Operation>,
    active_op_key: Option<String>,
    op_cancel: Option<watch::Sender<bool>>,
    op_done_tx: mpsc::UnboundedSender<OpId>,
    op_done_rx: mpsc::UnboundedReceiver<OpId>,
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
    let (op_done_tx, op_done_rx) = mpsc::unbounded_channel::<OpId>();

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
        active_op: None,
        active_op_key: None,
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
                        Some(_op_id) => {
                            self.active_op = None;
                            self.active_op_key = None;
                            self.op_cancel = None;
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
                        SupervisorCommand::CancelOperation { op_id, ack } => {
                            let cancelled = match &self.active_op {
                                Some(active) if active.op_id == op_id => {
                                    if let Some(cancel_tx) = &self.op_cancel {
                                        let _ = cancel_tx.send(true);
                                        true
                                    } else {
                                        false
                                    }
                                }
                                _ => false,
                            };
                            let _ = ack.send(Ok(cancelled));
                        }
                        SupervisorCommand::GetActiveOp { ack } => {
                            let _ = ack.send(self.active_op.clone());
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
        if let Some(active) = &self.active_op {
            if key.is_some() && key == self.active_op_key {
                let _ = ack.send(Ok(Some(active.op_id.clone())));
                return;
            }
            let _ = ack.send(Err(DaemonError::OperationAlreadyRunning {
                instance_id: self.id,
                active_op_id: active.op_id.clone(),
                active_kind: active.kind,
            }));
            return;
        }

        let op_id = op.op_id.clone();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let events = self.events.clone();
        let audit_dir = self.audit_dir.clone();
        let op_done_tx = self.op_done_tx.clone();
        self.active_op = Some(op.clone());
        self.active_op_key = key;
        self.op_cancel = Some(cancel_tx);

        tokio::spawn(async move {
            let mut progress = OpProgress::new(
                op.kind,
                op.instance_id,
                op.op_id,
                op.phases,
                events,
                audit_dir,
                cancel_rx,
            );
            progress.mark_running();
            // The operation itself calls progress.finish() (it owns the
            // handle); the sub-task only forwards the result.
            let result = run(progress).await;
            let _ = done_tx.send(result);
            let _ = op_done_tx.send(op_id);
        });
        let _ = ack.send(Ok(None));
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
    }
}
