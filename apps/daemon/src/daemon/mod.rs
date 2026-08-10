mod audit;
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
    BackendError, BackendHandle, BackendKind, DaemonEvent, HypervisorBackend, InstanceEvent,
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
    /// Registry entries whose instance.toml is missing/unreadable; kept so
    /// list/resolve/remove still see them, with the reason attached.
    pub(crate) broken: RwLock<HashMap<InstanceId, String>>,
    pub(crate) store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
}

const EVENT_CHANNEL_CAPACITY: usize = 256;

impl Daemon {
    // consumed by the upcoming events-RPC work and tests
    #[allow(dead_code)]
    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }
}

impl Daemon {
    pub fn new() -> Self {
        Self::with_backends_and_store(default_backends(), None)
    }

    pub async fn restore(store: Store) -> Result<Self, DaemonError> {
        Self::restore_with_root(store, andler_core::paths::instances_root()).await
    }

    /// File-based registry builder. `instances_root` is parameterized for
    /// tests; production goes through [`Self::restore`].
    pub async fn restore_with_root(
        store: Store,
        instances_root: std::path::PathBuf,
    ) -> Result<Self, DaemonError> {
        let backends = default_backends();
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut supervisors = HashMap::new();
        let mut broken: HashMap<InstanceId, String> = HashMap::new();

        // Legacy migration (pre-phase-1 databases): write each stored config
        // out as instance.toml. A file that already exists and differs is an
        // ambiguity — refuse loudly instead of silently picking one.
        let legacy = store.load_legacy_instances().await?;
        let migrated = legacy.len();
        for cfg in &legacy {
            let id = cfg.id;
            let dir = instances_root.join(id.to_string());
            let toml_path = dir.join("instance.toml");
            if toml_path.exists() {
                let content =
                    tokio::fs::read_to_string(&toml_path)
                        .await
                        .map_err(|err| DaemonError::Io {
                            path: toml_path.clone(),
                            source: err,
                        })?;
                match andler_core::config::parse_instance_config_toml(&content) {
                    Ok(file_cfg) if file_cfg == *cfg => {}
                    Ok(_) | Err(_) => {
                        return Err(DaemonError::ConfigMigrationConflict {
                            instance_id: id,
                            file_path: toml_path,
                        });
                    }
                }
            } else {
                andler_core::paths::ensure_private_dir(&dir)
                    .await
                    .map_err(|err| DaemonError::Io {
                        path: dir.clone(),
                        source: err,
                    })?;
                types::write_instance_toml(&dir, cfg).await;
            }
        }
        if !legacy.is_empty() {
            store.finalize_config_migration().await?;
            tracing::info!(
                migrated,
                "migrated instance configs from the legacy store to instance.toml"
            );
        }

        // Directory scan: every instance is a directory under
        // ~/.andler/instances/<id> whose instance.toml is the source of
        // truth. Unreadable entries are tracked as broken — listed with the
        // reason and removable — never fatal to daemon startup. The root
        // itself is created (0700) when missing, e.g. a fresh ANDLER_HOME.
        andler_core::paths::ensure_private_dir(&instances_root)
            .await
            .map_err(|err| DaemonError::Io {
                path: instances_root.clone(),
                source: err,
            })?;
        let mut entries =
            tokio::fs::read_dir(&instances_root)
                .await
                .map_err(|err| DaemonError::Io {
                    path: instances_root.clone(),
                    source: err,
                })?;
        while let Some(entry) = entries.next_entry().await.map_err(|err| DaemonError::Io {
            path: instances_root.clone(),
            source: err,
        })? {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(id) = name.parse::<InstanceId>() else {
                continue;
            };
            if supervisors.contains_key(&id) || broken.contains_key(&id) {
                continue;
            }

            let dir = entry.path();
            let toml_path = dir.join("instance.toml");
            let content = match tokio::fs::read_to_string(&toml_path).await {
                Ok(content) => content,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    broken.insert(
                        id,
                        "instance.toml is missing — was the instance directory tampered \
                         with? `andler remove --purge` can clean it up"
                            .to_string(),
                    );
                    continue;
                }
                Err(err) => {
                    broken.insert(id, format!("cannot read instance.toml: {err}"));
                    continue;
                }
            };
            let cfg = match andler_core::config::parse_instance_config_toml(&content) {
                Ok(cfg) => cfg,
                Err(err) => {
                    broken.insert(
                        id,
                        format!(
                            "invalid instance.toml ({err}) — fix the file or `andler remove \
                             --purge` the directory"
                        ),
                    );
                    continue;
                }
            };
            if cfg.id != id {
                broken.insert(
                    id,
                    "instance.toml belongs to a different instance id — fix the file or \
                     `andler remove --purge` the directory"
                        .to_string(),
                );
                continue;
            }

            // Adopt any QEMU process that survived the daemon restart; no
            // socket means no process and the instance is simply stopped.
            let mut state = InstanceState::Stopped;
            let mut recovered_handle: Option<BackendHandle> = None;
            let backend = backends
                .get(&cfg.backend)
                .ok_or_else(|| DaemonError::NoBackendRegistered(cfg.backend))?;
            match backend.adopt(&cfg).await {
                Ok((backend_handle, actual_state)) => {
                    tracing::info!(
                        instance_id = %id,
                        ?actual_state,
                        "reconnected to a QEMU process that survived the daemon restart"
                    );
                    recovered_handle = Some(backend_handle);
                    state = actual_state;
                }
                Err(adopt_err) => {
                    tracing::debug!(
                        instance_id = %id,
                        error = %adopt_err,
                        "no live backend process to adopt; instance starts stopped"
                    );
                }
            }

            let handle = spawn_supervisor(id, cfg, state, recovered_handle.clone(), events.clone());
            if let Some(backend_handle) = recovered_handle {
                let backend = backends
                    .get(&handle.config().backend)
                    .expect("supervisor config backend must be registered");
                Self::attach_process_exit_watcher(id, &handle, backend.clone(), backend_handle);
            }
            supervisors.insert(id, handle);
        }

        Ok(Self {
            backends,
            supervisors: RwLock::new(supervisors),
            broken: RwLock::new(broken),
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
            broken: RwLock::new(HashMap::new()),
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
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
