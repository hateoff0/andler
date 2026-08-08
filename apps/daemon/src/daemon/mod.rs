mod clone_ops;
mod error;
mod health_ops;
mod hotplug_ops;
mod instance_ops;
mod query_ops;
mod snapshot_ops;
mod types;

pub use error::{DaemonError, ErrorKind};
pub(crate) use types::InstanceRecord;

use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendKind, DaemonEvent, EventKind, HypervisorBackend, InstanceConfig,
    InstanceEvent, InstanceId, InstanceState,
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
    pub(crate) instances: RwLock<HashMap<InstanceId, InstanceRecord>>,
    pub(crate) store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
}

const EVENT_CHANNEL_CAPACITY: usize = 256;

impl Daemon {
    // consumed by the supervisor (next Phase 0 commit) and the events RPC
    #[allow(dead_code)]
    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    /// Publishes a daemon event; drops silently when no subscriber is
    /// listening (the audit file in Phase 1 subscribes and persists).
    pub(crate) fn emit(&self, instance_id: Option<InstanceId>, kind: EventKind) {
        let event = DaemonEvent {
            ts_ms: chrono::Utc::now().timestamp_millis() as u64,
            instance_id,
            kind,
        };
        let _ = self.events.send(event);
    }

    pub(crate) fn emit_lifecycle(
        &self,
        id: InstanceId,
        from: InstanceState,
        to: InstanceState,
        reason: Option<String>,
    ) {
        self.emit(Some(id), EventKind::Lifecycle { from, to, reason });
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

        let mut instances = HashMap::with_capacity(stored.len());
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

            instances.insert(
                id,
                InstanceRecord {
                    config: entry.config,
                    state,
                    handle: None,
                },
            );
        }

        Ok(Self::with_backends_and_store_and_instances(
            default_backends(),
            Some(store),
            instances,
        ))
    }

    fn with_backends_and_store(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
    ) -> Self {
        Self::with_backends_and_store_and_instances(backends, store, HashMap::new())
    }

    fn with_backends_and_store_and_instances(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
        instances: HashMap<InstanceId, InstanceRecord>,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Daemon {
            backends,
            instances: RwLock::new(instances),
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

    /// Applies an FSM event and persists the result, logging (but not failing the
    /// caller's overall operation on) any error — used after an action that already
    /// succeeded at the backend level, where the point is just to keep our own
    /// bookkeeping in sync, not to gate the action itself.
    pub(crate) async fn apply_event_and_persist(&self, id: InstanceId, event: InstanceEvent) {
        let (from, new_state) = {
            let mut instances = self.instances.write().await;
            let Some(record) = instances.get_mut(&id) else {
                tracing::error!(instance_id = %id, ?event, "instance vanished before FSM event could be applied");
                return;
            };
            match record.state.clone().apply(event.clone()) {
                Ok(state) => {
                    let from = record.state.clone();
                    record.state = state.clone();
                    (from, state)
                }
                Err(err) => {
                    tracing::error!(instance_id = %id, ?event, error = %err, "failed to apply FSM event");
                    return;
                }
            }
        };
        let reason = match &event {
            InstanceEvent::Fail(message) => Some(message.clone()),
            _ => None,
        };
        self.emit_lifecycle(id, from, new_state.clone(), reason);
        self.persist_state(id, &new_state).await;
    }

    /// Returns (backend, handle) for an instance that has been spawned (handle present),
    /// regardless of its current lifecycle state.
    pub(crate) async fn backend_and_handle(
        &self,
        id: InstanceId,
    ) -> Result<(Arc<dyn HypervisorBackend>, andler_core::BackendHandle), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.to_string())))?;
        let backend = self.backend_for(record.config.backend)?.clone();

        Ok((backend, handle))
    }

    /// Returns (backend, handle) for an instance in Running or Paused state.
    /// Used by snapshot operations, which require the instance to be running or paused.
    pub(crate) async fn with_running_instance(
        &self,
        id: InstanceId,
    ) -> Result<(Arc<dyn HypervisorBackend>, andler_core::BackendHandle), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        if !matches!(record.state, InstanceState::Running | InstanceState::Paused) {
            return Err(DaemonError::SnapshotOperationRequiresRunningInstance(
                id,
                record.state.clone(),
            ));
        }

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.to_string())))?;
        let backend = self.backend_for(record.config.backend)?.clone();

        Ok((backend, handle))
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

    pub(crate) async fn persist_config_update(&self, cfg: &InstanceConfig, state: &InstanceState) {
        if let Some(dir) = cfg.disk.path.parent() {
            types::write_instance_toml(dir, cfg).await;
        }

        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.save_instance(cfg, state).await {
            tracing::error!(
                instance_id = %cfg.id,
                error = %err,
                "failed to persist updated instance config to store"
            );
        }
    }

    pub(crate) async fn persist_state(&self, id: InstanceId, state: &InstanceState) {
        let cfg = {
            let instances = self.instances.read().await;
            match instances.get(&id) {
                Some(record) => record.config.clone(),
                None => return,
            }
        };
        self.persist_config_update(&cfg, state).await;
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
