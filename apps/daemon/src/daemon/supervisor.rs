use andler_core::{
    BackendHandle, DaemonEvent, EventKind, FsmError, InstanceConfig, InstanceEvent, InstanceId,
    InstanceState,
};
use andler_store::Store;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::error::DaemonError;

/// One task per instance owns the instance's FSM state, backend handle and
/// config; everything else in the daemon reads those through watch snapshots
/// and mutates them through commands. This removes the structural RwLock
/// contention of the old shared map and gives the daemon a single place to
/// grow op-queueing and event sourcing onto.
#[derive(Clone)]
pub(crate) struct SupervisorHandle {
    pub(crate) id: InstanceId,
    pub(crate) cmd_tx: mpsc::Sender<SupervisorCommand>,
    pub(crate) state_rx: watch::Receiver<InstanceState>,
    pub(crate) handle_rx: watch::Receiver<Option<BackendHandle>>,
    pub(crate) config_rx: watch::Receiver<InstanceConfig>,
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
        ack: oneshot::Sender<Result<(), ()>>,
    },
    Shutdown,
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

    pub(crate) async fn set_config(&self, config: InstanceConfig) -> Result<(), DaemonError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::SetConfig {
                config,
                ack: ack_tx,
            })
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        let result = ack_rx
            .await
            .map_err(|_| DaemonError::InstanceSupervisorGone(self.id))?;
        result.map_err(|_| DaemonError::InstanceSupervisorGone(self.id))
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
    store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
    state_tx: watch::Sender<InstanceState>,
    handle_tx: watch::Sender<Option<BackendHandle>>,
    config_tx: watch::Sender<InstanceConfig>,
    inbox: mpsc::Receiver<SupervisorCommand>,
}

pub(crate) fn spawn_supervisor(
    id: InstanceId,
    config: InstanceConfig,
    state: InstanceState,
    handle: Option<BackendHandle>,
    store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
) -> SupervisorHandle {
    let (cmd_tx, inbox) = mpsc::channel(64);
    let (state_tx, state_rx) = watch::channel(state.clone());
    let (handle_tx, handle_rx) = watch::channel(handle.clone());
    let (config_tx, config_rx) = watch::channel(config.clone());

    let supervisor = InstanceSupervisor {
        id,
        state,
        handle,
        config,
        store,
        events,
        state_tx,
        handle_tx,
        config_tx,
        inbox,
    };
    tokio::spawn(async move {
        supervisor.run().await;
    });

    SupervisorHandle {
        id,
        cmd_tx,
        state_rx,
        handle_rx,
        config_rx,
    }
}

impl InstanceSupervisor {
    async fn run(mut self) {
        while let Some(command) = self.inbox.recv().await {
            match command {
                SupervisorCommand::Transition { event, ack } => {
                    self.apply_transition(event, ack).await;
                }
                SupervisorCommand::SetHandle { handle, ack } => {
                    self.handle = handle.clone();
                    let _ = self.handle_tx.send(handle);
                    let _ = ack.send(Ok(()));
                }
                SupervisorCommand::SetConfig { config, ack } => {
                    self.config = config.clone();
                    let _ = self.config_tx.send(config);
                    self.persist().await;
                    let _ = ack.send(Ok(()));
                }
                SupervisorCommand::Shutdown => break,
            }
        }
        tracing::debug!(instance_id = %self.id, "instance supervisor stopped");
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
                let _ = self.events.send(event);
                let _ = ack.send(Ok(new_state));
            }
            Err(err) => {
                let _ = ack.send(Err(err));
            }
        }
    }

    async fn persist(&self) {
        let instance_dir = self.config.disk.path.parent();
        if let Some(dir) = instance_dir {
            super::types::write_instance_toml(dir, &self.config).await;
        }
        if let Some(store) = &self.store {
            if let Err(err) = store.save_instance(&self.config, &self.state).await {
                tracing::error!(
                    instance_id = %self.id,
                    error = %err,
                    "failed to persist instance state to store"
                );
            }
        }
    }
}
