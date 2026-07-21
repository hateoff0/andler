

mod error;
mod types;
mod snapshot_ops;
mod query_ops;
mod instance_ops;
mod clone_ops;
mod health_ops;

pub use error::DaemonError;
pub(crate) use types::InstanceRecord;

use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendKind, HypervisorBackend, InstanceConfig,
    InstanceId, InstanceState,
};
use andler_qemu::QemuBackend;
use andler_store::Store;
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
                state @ (InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }) => {
                    state
                }
                lost_state @ (InstanceState::Starting
                | InstanceState::Running
                | InstanceState::Paused
                | InstanceState::Stopping) => {
                    tracing::warn!(
                        instance_id = %id.0,
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
        Daemon {
            backends,
            instances: RwLock::new(instances),
            store,
        }
    }

    pub(crate) fn backend_for(&self, kind: BackendKind) -> Result<&Arc<dyn HypervisorBackend>, DaemonError> {
        self.backends
            .get(&kind)
            .ok_or(DaemonError::NoBackendRegistered(kind))
    }


    pub(crate) async fn persist_new_instance(&self, cfg: &InstanceConfig, state: &InstanceState) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.save_instance(cfg, state).await {
            tracing::error!(
                instance_id = %cfg.id.0,
                error = %err,
                "failed to persist new instance to store"
            );
        }
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
                instance_id = %cfg.id.0,
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
