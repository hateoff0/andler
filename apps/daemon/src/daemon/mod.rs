mod audit;
mod clone_ops;
mod disk_chain;
mod error;
mod export_ops;
mod health_ops;
mod hotplug_ops;
mod instance_ops;
mod ops;
mod query_ops;
mod snapshot_ops;
mod supervisor;
mod types;

pub use error::{DaemonError, ErrorKind};
pub(crate) use supervisor::{spawn_supervisor, SupervisorHandle};

use futures_core::stream::BoxStream;
use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendHandle, BackendKind, DaemonEvent, EventKind, HypervisorBackend,
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
    pub(crate) supervisors: std::sync::Arc<RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    /// Registry entries whose instance.toml is missing/unreadable; kept so
    /// list/resolve/remove still see them, with the reason attached.
    pub(crate) broken: RwLock<HashMap<InstanceId, String>>,
    pub(crate) store: Option<Store>,
    events: broadcast::Sender<DaemonEvent>,
    /// Deferred-start tasks for instances marked `autostart`. Each task is
    /// supervised by a paired watcher that logs panic; we hold the
    /// JoinHandle so a panic is observed (AGENTS tokio::spawn rule).
    autostart_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Serializes the check→resource-claim critical section of every
    /// start across all instances (§H multi-instance): the four
    /// per-start resource gates (host port, disk path, pinned CPU, guest
    /// RAM) only count Running/Paused/Starting peers, so two instances
    /// still in Created and started concurrently would both pass the
    /// pairwise checks and both spawn against the same disk/port. Holding
    /// this lock across check→Start transition makes the first instance
    /// own its resources the moment it leaves Created, so a parallel
    /// start's checks see it as active and refuse. Held only across the
    /// fast check+transition, never across the (slow) spawn.
    start_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Forwards QMP events from every backend's event channel onto the daemon
/// bus as `DaemonEvent::Qmp`, resolving the backend handle to the instance
/// id. Runs for the daemon's lifetime; each relay task ends when its
/// backend drops (daemon teardown).
fn spawn_qmp_relays(
    daemon_events: broadcast::Sender<DaemonEvent>,
    backends: &HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    supervisors: std::sync::Arc<RwLock<HashMap<InstanceId, SupervisorHandle>>>,
) {
    for backend in backends.values() {
        let Some(mut rx) = backend.subscribe_qmp_events() else {
            continue;
        };
        let events = daemon_events.clone();
        let supervisors = supervisors.clone();
        tokio::spawn(async move {
            while let Ok(record) = rx.recv().await {
                let instance_id = {
                    let map = supervisors.read().await;
                    map.iter()
                        .find(|(_, handle)| handle.backend_handle() == Some(record.handle.clone()))
                        .map(|(id, _)| *id)
                };
                if let Some(instance_id) = instance_id {
                    let event = DaemonEvent {
                        ts_ms: chrono::Utc::now().timestamp_millis() as u64,
                        instance_id: Some(instance_id),
                        kind: EventKind::Qmp {
                            event: record.event,
                            data: record.data,
                        },
                    };
                    tracing::debug!(instance_id = %instance_id, event = ?record.event, "qmp event");
                    let _ = events.send(event);
                }
            }
        });
    }
}

impl Daemon {
    // Live event-bus subscription for the daemon's own tests (events.rs);
    // the StreamEvents RPC goes through stream_events below.
    #[allow(dead_code)]
    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    /// Streams daemon events (live bus), optionally filtered to one
    /// instance. Backs the `StreamEvents` RPC / `andler events`.
    pub fn stream_events(&self, filter: Option<InstanceId>) -> BoxStream<'static, DaemonEvent> {
        let mut rx = self.events.subscribe();
        Box::pin(async_stream::stream! {
            while let Ok(event) = rx.recv().await {
                match &filter {
                    Some(id) if event.instance_id.as_ref() != Some(id) => continue,
                    _ => yield event,
                }
            }
        })
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
            // A directory marked by a non-purge remove keeps its preserved
            // files but is deliberately not an instance anymore; the
            // missing toml would otherwise resurface it as broken on every
            // daemon restart.
            if dir.join("instance.removed").exists() {
                continue;
            }
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

            let handle = spawn_supervisor(
                id,
                dir.clone(),
                cfg.clone(),
                state.clone(),
                recovered_handle.clone(),
                events.clone(),
            );
            if let Some(backend_handle) = recovered_handle {
                let backend = backends
                    .get(&handle.config().backend)
                    .expect("supervisor config backend must be registered");
                Self::attach_process_exit_watcher(id, &handle, backend.clone(), backend_handle);
            }
            supervisors.insert(id, handle);

            // Reconcile the disk chain against snapshot metadata. When a
            // QEMU process was adopted and is running, only metadata is
            // touched (no file moves under a live VM).
            if matches!(cfg.disk.format, andler_core::DiskFormat::Qcow2) {
                let files = !matches!(state, InstanceState::Running | InstanceState::Paused);
                if let Err(err) =
                    disk_chain::reconcile_chain(&store, id, &dir, &cfg.disk.path, files).await
                {
                    tracing::warn!(
                        instance_id = %id,
                        error = %err,
                        "disk chain reconciliation failed"
                    );
                }
            }
        }

        let daemon = Daemon {
            backends,
            supervisors: std::sync::Arc::new(RwLock::new(supervisors)),
            broken: RwLock::new(broken),
            store: Some(store),
            events,
            autostart_tasks: std::sync::Mutex::new(Vec::new()),
            start_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        };
        spawn_qmp_relays(
            daemon.events.clone(),
            &daemon.backends,
            daemon.supervisors.clone(),
        );
        daemon.spawn_autostart_tasks().await;
        Ok(daemon)
    }

    fn with_backends_and_store(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let daemon = Daemon {
            backends,
            supervisors: std::sync::Arc::new(RwLock::new(HashMap::new())),
            broken: RwLock::new(HashMap::new()),
            store,
            events,
            autostart_tasks: std::sync::Mutex::new(Vec::new()),
            start_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        };
        spawn_qmp_relays(
            daemon.events.clone(),
            &daemon.backends,
            daemon.supervisors.clone(),
        );
        daemon
    }

    /// Deferred-start pass for instances marked `autostart`.
    /// Issues exactly one `start_instance` per matching instance via the
    /// shared `do_start_instance` path — never retried in a loop on
    /// failure, so a failing autostart leaves the instance in
    /// Error/Stopped and the daemon keeps running. Each task is paired
    /// with a watcher that observes its JoinHandle (AGENTS tokio::spawn
    /// rule: hold the handle, log panic/completion).
    async fn spawn_autostart_tasks(&self) {
        let supervisors = self.supervisors.clone();
        let backends = self.backends.clone();
        let start_lock = self.start_lock.clone();
        let ids: Vec<InstanceId> = {
            let map = supervisors.read().await;
            map.iter()
                .filter(|(_, handle)| {
                    handle.config().autostart && handle.state() == InstanceState::Stopped
                })
                .map(|(id, _)| *id)
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        tracing::info!(
            count = ids.len(),
            "scheduling autostart for instances marked autostart=true"
        );
        let join = tokio::spawn(async move {
            for id in ids {
                match instance_ops::do_start_instance(&start_lock, &supervisors, &backends, id)
                    .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!(
                            instance_id = %id,
                            error = %err,
                            "autostart failed; instance left in Error/Stopped, daemon continues"
                        );
                    }
                }
            }
        });
        let watcher = tokio::spawn(async move {
            match join.await {
                Ok(()) => {}
                Err(join_err) if join_err.is_panic() => {
                    tracing::error!(error = ?join_err, "autostart task panicked");
                }
                Err(join_err) => {
                    tracing::warn!(error = ?join_err, "autostart task cancelled");
                }
            }
        });
        // Keep both handles; the watcher's handle is the canonical one
        // we retain to satisfy the AGENTS rule (observe completion).
        // The inner JoinHandle is owned by the watcher and is observed
        // through its result above.
        if let Ok(mut guard) = self.autostart_tasks.lock() {
            guard.push(watcher);
        } else {
            tracing::warn!("autostart_tasks mutex poisoned; dropping watcher handle");
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
