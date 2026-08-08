mod clone_ops;
mod error;
mod health_ops;
mod hotplug_ops;
mod instance_ops;
mod query_ops;
mod snapshot_ops;
mod supervisor;
mod types;

pub use error::{DaemonError, ErrorKind};
pub(crate) use supervisor::{spawn_supervisor, SupervisorHandle};

use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendKind, DaemonEvent, HypervisorBackend, InstanceConfig, InstanceEvent,
    InstanceId, InstanceState,
};
use andler_qemu::QemuBackend;
use andler_store::Store;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

fn default_backends() -> HashMap<BackendKind, Arc<dyn HypervisorBackend>> {
    let mut backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>> = HashMap::new();
    backends.insert(BackendKind::Qemu, Arc::new(QemuBackend::new()));
    backends
}

pub struct Daemon {
    pub(crate) backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    pub(crate) supervisors: RwLock<HashMap<InstanceId, SupervisorHandle>>,
    pub(crate) store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
}

const EVENT_CHANNEL_CAPACITY: usize = 256;

impl Daemon {
    // consumed by the supervisor-next Phase 0 commit (events RPC) and tests
    #[allow(dead_code)]
    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }
}

impl Daemon {
    pub fn new() -> Self {
        Self::with_backends_and_store(default_backends(), None)
    }

    #[cfg(test)]
    pub fn with_store(store: Store) -> Self {
        Self::with_backends_and_store(default_backends(), Some(store))
    }

    pub async fn restore(store: Store) -> Result<Self, DaemonError> {
        let stored = store.load_all().await?;

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut supervisors = HashMap::with_capacity(stored.len());
        for entry in stored {
            let id = entry.config.id;
            let state = match entry.state {
                state @ (InstanceState::Created
                | InstanceState::Stopped
                | InstanceState::Error { .. }) => state,
                lost_state @ (InstanceState::Starting
                | InstanceState::Running
                | InstanceState::Paused
                | InstanceState::Stopping) => {
                    tracing::warn!(
                        instance_id = %id,
                        previous_state = ?lost_state,
                        "restored instance was not in a terminal state before restart; \
                         backend handle cannot be recovered, marking as Error"
                    );
                    InstanceState::Error {
                        message: format!(
                            "andlerd restarted while instance was in state {lost_state:?}; \
                             backend handle was not persisted and cannot be recovered, \
                             instance must be restarted explicitly"
                        ),
                    }
                }
            };
            let handle = spawn_supervisor(
                id,
                entry.config,
                state,
                None,
                Some(store.clone()),
                events.clone(),
            );
            supervisors.insert(id, handle);
        }

        Ok(Self {
            backends: default_backends(),
            supervisors: RwLock::new(supervisors),
            store: Some(store),
            events,
        })
    }

    fn with_backends_and_store(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Daemon {
            backends,
            supervisors: RwLock::new(HashMap::new()),
            store,
            events,
        }
    }

    pub(crate) fn backend_for(
        &self,
        kind: BackendKind,
    ) -> Result<&Arc<dyn HypervisorBackend>, DaemonError> {
        self.backends
            .get(&kind)
            .ok_or(DaemonError::NoBackendRegistered(kind))
    }

    pub(crate) async fn handle_for(&self, id: InstanceId) -> Result<SupervisorHandle, DaemonError> {
        let supervisors = self.supervisors.read().await;
        supervisors
            .get(&id)
            .cloned()
            .ok_or(DaemonError::InstanceNotFound(id))
    }

    pub(crate) fn event_sender(&self) -> broadcast::Sender<DaemonEvent> {
        self.events.clone()
    }

    /// Applies an FSM event via the instance supervisor and persists the
    /// result, logging (but not failing the caller's overall operation on)
    /// any error — used after an action that already succeeded at the backend
    /// level, where the point is just to keep our own bookkeeping in sync,
    /// not to gate the action itself.
    pub(crate) async fn apply_event_and_persist(&self, id: InstanceId, event: InstanceEvent) {
        let Some(handle) = self.supervisors.read().await.get(&id).cloned() else {
            tracing::error!(instance_id = %id, ?event, "instance vanished before FSM event could be applied");
            return;
        };
        if let Err(err) = handle.transition(event).await {
            tracing::error!(instance_id = %id, error = %err, "failed to apply FSM event");
        }
    }

    /// Returns (backend, handle) for an instance that has been spawned (handle present),
    /// regardless of its current lifecycle state.
    pub(crate) async fn backend_and_handle(
        &self,
        id: InstanceId,
    ) -> Result<(Arc<dyn HypervisorBackend>, andler_core::BackendHandle), DaemonError> {
        let handle = self.handle_for(id).await?;

        let backend_handle = handle
            .backend_handle()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.to_string())))?;
        let backend = self.backend_for(handle.config().backend)?.clone();

        Ok((backend, backend_handle))
    }

    /// Returns (backend, handle) for an instance in Running or Paused state.
    /// Used by snapshot operations, which require the instance to be running or paused.
    pub(crate) async fn with_running_instance(
        &self,
        id: InstanceId,
    ) -> Result<(Arc<dyn HypervisorBackend>, andler_core::BackendHandle), DaemonError> {
        let handle = self.handle_for(id).await?;

        let state = handle.state();
        if !matches!(state, InstanceState::Running | InstanceState::Paused) {
            return Err(DaemonError::SnapshotOperationRequiresRunningInstance(
                id, state,
            ));
        }

        let backend_handle = handle
            .backend_handle()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.to_string())))?;
        let backend = self.backend_for(handle.config().backend)?.clone();

        Ok((backend, backend_handle))
    }

    pub(crate) async fn persist_new_instance(
        &self,
        cfg: &InstanceConfig,
        state: &InstanceState,
    ) -> Result<(), DaemonError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store.save_instance(cfg, state).await.map_err(|err| {
            tracing::error!(
                instance_id = %cfg.id,
                error = %err,
                "failed to persist new instance to store"
            );
            DaemonError::Store(err)
        })
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
