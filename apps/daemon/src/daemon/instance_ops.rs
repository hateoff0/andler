use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::error::DaemonError;
use super::ops::OpProgress;
use super::spawn_supervisor;
use super::supervisor::{OpAccept, OpRunner};
use super::types::{write_instance_toml, InstanceDirGuard};
use super::{Daemon, SupervisorHandle};
use andler_core::{
    BackendError, BackendHandle, BackendKind, DaemonEvent, DiskFormat, EventKind, EventLogLevel,
    GuestMutator, HypervisorBackend, InstanceConfig, InstanceEvent, InstanceId, InstanceKind,
    InstanceState, MutatorOp, Operation, OperationKind, OperationState, Resolution,
    INSTANCE_ID_HEX_LEN,
};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

/// Checks that the files this instance needs to boot are still present on disk.
/// Catches the case where the instance directory was deleted or moved outside
/// andler's knowledge — without this, `spawn()` would fork qemu-system-x86_64
/// successfully (the OS-level exec succeeds regardless), report the instance as
/// Running, and only reveal the real failure a health-check cycle later.
fn validate_instance_files(cfg: &InstanceConfig) -> Result<(), BackendError> {
    if !cfg.disk.path.exists() {
        return Err(BackendError::Io(format!(
            "disk file not found: {} (was the instance directory moved or deleted?)",
            cfg.disk.path.display()
        )));
    }

    if cfg.firmware.enable_uefi && !cfg.firmware.ovmf_vars_path.exists() {
        return Err(BackendError::Io(format!(
            "OVMF_VARS file not found: {} (was the instance directory moved or deleted?)",
            cfg.firmware.ovmf_vars_path.display()
        )));
    }

    for disk in &cfg.extra_disks {
        if !disk.path.exists() {
            return Err(BackendError::Io(format!(
                "extra disk file not found: {} (attached disk was moved or deleted?)",
                disk.path.display()
            )));
        }
    }

    Ok(())
}

impl Daemon {
    pub async fn resolve_instance_id(&self, raw: &str) -> Result<InstanceId, DaemonError> {
        if raw.is_empty() {
            return Err(DaemonError::EmptyInstanceRef);
        }

        let needle = raw.to_ascii_lowercase();
        if !needle.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DaemonError::MalformedInstanceRef(raw.to_string()));
        }

        if needle.len() == INSTANCE_ID_HEX_LEN {
            return needle
                .parse()
                .map_err(|_| DaemonError::MalformedInstanceRef(raw.to_string()));
        }

        let supervisors = self.supervisors.read().await;
        let broken = self.broken.read().await;
        let mut matches: Vec<InstanceId> = supervisors
            .keys()
            .chain(broken.keys())
            .filter(|id| id.to_string().starts_with(&needle))
            .copied()
            .collect();
        matches.sort_by_key(|id| id.to_string());
        matches.dedup();

        match matches.len() {
            0 => Err(DaemonError::InstanceRefNotFound(raw.to_string())),
            1 => Ok(matches[0]),
            _ => Err(DaemonError::AmbiguousInstanceId {
                prefix: raw.to_string(),
                candidates: matches,
            }),
        }
    }

    /// Test/back-compat path: the registry directory is the disk's parent.
    /// Production registration goes through [`Self::create_instance_in`],
    /// which receives the registry directory explicitly.
    #[cfg(test)]
    pub async fn create_instance(&self, cfg: InstanceConfig) -> Result<InstanceId, DaemonError> {
        let instance_dir = cfg
            .disk
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default();
        self.create_instance_in(cfg, instance_dir).await
    }

    /// Registers the instance at the given registry directory, which owns
    /// instance.toml and events.jsonl. Callers that created the directory
    /// (create_linux/android, clone, restore scan) pass it explicitly — the
    /// config's disk path can point anywhere and must not be used to derive
    /// the registry directory.
    pub async fn create_instance_in(
        &self,
        cfg: InstanceConfig,
        instance_dir: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        Self::register_instance(
            &self.supervisors,
            &self.backends,
            &self.events,
            cfg,
            instance_dir,
        )
        .await
    }

    /// The registry-side half of registration, against the pieces a
    /// supervisor operation holds by value: an operation body runs in its
    /// own task and cannot borrow the daemon, so a clone that registers the
    /// instance it created needs the pieces directly. `create_instance_in`
    /// is the `&self` view of this — one implementation, two views.
    pub(crate) async fn register_instance(
        supervisors: &Arc<RwLock<HashMap<InstanceId, SupervisorHandle>>>,
        backends: &HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        events: &broadcast::Sender<DaemonEvent>,
        cfg: InstanceConfig,
        instance_dir: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        cfg.validate().map_err(DaemonError::InvalidConfig)?;
        if !backends.contains_key(&cfg.backend) {
            return Err(DaemonError::NoBackendRegistered(cfg.backend));
        }

        let id = cfg.id;
        let handle = spawn_supervisor(
            id,
            instance_dir,
            cfg.clone(),
            InstanceState::Created,
            None,
            events.clone(),
        );
        supervisors.write().await.insert(id, handle);

        let _ = events.send(DaemonEvent {
            ts_ms: chrono::Utc::now().timestamp_millis() as u64,
            instance_id: Some(id),
            kind: EventKind::Log {
                level: EventLogLevel::Info,
                message: format!("instance created ({})", cfg.name),
            },
        });

        tracing::info!(instance_id = %id, name = %cfg.name, "instance created");
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)] // mirrors the CreateAndroidInstanceRequest proto fields
    pub async fn create_android_instance(
        &self,
        mut profile: andler_core::AndroidProfile,
        instance_name: String,
        base_image_path: PathBuf,
        instances_root: PathBuf,
        overlay_size_bytes: u64,
        ovmf_vars_template: PathBuf,
        linked_overlay: bool,
    ) -> Result<InstanceId, DaemonError> {
        let id = InstanceId::new();
        let instance_dir = instances_root.join(id.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());

        // Pin check first — before any copy, overlay, or even the instance
        // directory exists. An existing pin that no longer matches means
        // the image on disk is not the one this instance was created from.
        if let Some(pin) =
            Self::verify_or_derive_pin(&base_image_path, profile.base_image_pin.as_ref())?
        {
            profile.base_image_pin = Some(pin);
        }

        andler_core::paths::ensure_private_dir(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        if ovmf_vars_template.as_os_str().is_empty() {
            return Err(DaemonError::MissingOvmfVarsTemplate);
        }

        let ovmf_vars_path = instance_dir.join("VARS.fd");
        andler_firmware::provision_vars(&ovmf_vars_template, &ovmf_vars_path)
            .await
            .map_err(|e| DaemonError::Firmware(e.to_string()))?;

        let disk = if linked_overlay {
            let overlay = andler_disk::overlay::create_overlay(
                &instance_dir,
                &base_image_path,
                overlay_size_bytes,
            )
            .await?;

            andler_core::DiskConfig::overlay(
                overlay.overlay_path,
                overlay.base_image_path,
                overlay_size_bytes,
            )
        } else {
            let disk_path = instance_dir.join("disk.qcow2");
            andler_disk::qcow2::clone_full(&base_image_path, &disk_path).await?;
            andler_disk::qcow2::resize(&disk_path, overlay_size_bytes, true).await?;

            andler_core::DiskConfig::standalone(disk_path, overlay_size_bytes)
        };

        // The base image's own baked-in default.target is multi-user.target (plain Linux),
        // since the same image can back both AndroidVm and LinuxVm instances. An AndroidVm
        // should boot into android.target — set that explicitly on the fresh disk now,
        // before it's ever started. Best-effort: if this fails (e.g. nbd module not loaded,
        // or the disk has no real partition table), log it and continue rather than blocking
        // instance creation entirely — the instance is still usable, just left on whatever
        // boot mode the base image defaults to, and can be fixed with `andler guest boot-mode`.
        if let Err(e) = andler_disk::boot_mode::switch_boot_mode_with(
            &andler_guestfs::GuestfsMutator::new(disk.path.clone()),
            andler_core::AndroidBootMode::Android,
        )
        .await
        {
            tracing::error!(
                error = %e,
                "could not set default boot mode to android on new instance disk; \
                 it will boot into whatever mode the base image defaults to until \
                 fixed with `andler guest boot-mode <id> android`"
            );
        }

        let mut cfg = profile.resolve(instance_name, disk, ovmf_vars_path);
        cfg.id = id;

        write_instance_toml(&instance_dir, &cfg).await;

        let registered_id = self.create_instance_in(cfg, instance_dir.clone()).await?;

        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Enforces or records the base-image pin. With an existing pin,
    /// creation refuses an image whose manifest id or content sha256 no
    /// longer matches — a silently swapped backing image corrupts every
    /// linked clone sitting on top of it. Without a pin, the freshly chosen
    /// image's id + sha256 are recorded so the instance file carries its
    /// provenance. The check is creation-time only: once the instance disk
    /// (or overlay) exists, its backing file reference — not the pin —
    /// determines what the VM actually reads, and re-hashing a multi-GB
    /// image on every start would cost seconds per boot for no protection.
    pub(crate) fn verify_or_derive_pin(
        base_image_path: &std::path::Path,
        pin: Option<&andler_core::BaseImagePin>,
    ) -> Result<Option<andler_core::BaseImagePin>, DaemonError> {
        let Some(info) = andler_core::base_image::info_for(base_image_path) else {
            if let Some(pin) = pin {
                return Err(DaemonError::BaseImagePinMismatch(format!(
                    "the instance file pins base image `{}`, but no manifest.json exists next \
                     to {}; the image's identity cannot be verified — restore the original \
                     image or remove `base_image_pin` from the instance file to accept the \
                     current one",
                    pin.id,
                    base_image_path.display()
                )));
            }
            tracing::warn!(
                path = %base_image_path.display(),
                "no manifest.json next to the base image; pin not recorded"
            );
            return Ok(None);
        };

        let id = info.id();
        match pin {
            Some(pin) => {
                if pin.id != id {
                    return Err(DaemonError::BaseImagePinMismatch(format!(
                        "the instance file pins base image `{}`, but {} is `{}`; \
                         the image was swapped since the instance was created — restore \
                         the original image or remove `base_image_pin` from the instance \
                         file to accept the new one",
                        pin.id,
                        base_image_path.display(),
                        id
                    )));
                }
                let actual = andler_core::base_image::sha256_of(base_image_path)?;
                if actual != pin.sha256 {
                    return Err(DaemonError::BaseImagePinMismatch(format!(
                        "base image {} changed since it was pinned (expected sha256 {}, \
                         got {}); the instance's linked clones may read different content \
                         than at creation — restore the original image or remove \
                         `base_image_pin` from the instance file to accept the new one",
                        base_image_path.display(),
                        pin.sha256,
                        actual
                    )));
                }
                Ok(None)
            }
            None => {
                let sha256 = andler_core::base_image::sha256_of(base_image_path)?;
                Ok(Some(andler_core::BaseImagePin { id, sha256 }))
            }
        }
    }

    pub async fn create_linux_instance(
        &self,
        mut cfg: InstanceConfig,
        instances_root: PathBuf,
        ovmf_vars_template: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        let id = InstanceId::new();
        let instance_dir = instances_root.join(id.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());

        andler_core::paths::ensure_private_dir(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        if !ovmf_vars_template.as_os_str().is_empty() {
            let ovmf_vars_path = instance_dir.join("VARS.fd");
            andler_firmware::provision_vars(&ovmf_vars_template, &ovmf_vars_path)
                .await
                .map_err(|e| DaemonError::Firmware(e.to_string()))?;
            cfg.firmware.ovmf_vars_path = ovmf_vars_path;
        }

        if cfg.disk.format == DiskFormat::Qcow2 && !cfg.disk.path.exists() {
            let disk_file_name = cfg
                .disk
                .path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("disk.qcow2"));
            cfg.disk.path = instance_dir.join(disk_file_name);

            andler_disk::qcow2::create(&cfg.disk.path, cfg.disk.size_bytes)
                .await
                .map_err(DaemonError::Disk)?;
        }

        cfg.id = id;

        write_instance_toml(&instance_dir, &cfg).await;

        let registered_id = self.create_instance_in(cfg, instance_dir).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    pub async fn start_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        do_start_instance(&self.start_lock, &self.supervisors, &self.backends, id).await
    }

    /// Refuses to start an instance whose `network.port_forwards` host
    /// ports are already forwarded by another instance that is running
    /// (or starting) right now. Two QEMU slirp netdevs bound to the same
    /// host port would otherwise fail with an opaque QEMU error at best
    /// and silently misroute traffic at worst — the host port is a shared
    /// resource across instances.
    async fn check_port_forward_conflicts(
        &self,
        id: InstanceId,
        cfg: &InstanceConfig,
    ) -> Result<(), DaemonError> {
        check_port_forward_conflicts_impl(&self.supervisors, id, cfg).await
    }

    /// Watches the backend's process-exit stream and turns an unexpected
    /// QEMU death into an immediate supervised Fail — but only while the
    /// instance is Running/Paused/Starting; a deliberate stop owns its own
    /// outcome and must not be raced by this task.
    pub(crate) fn attach_process_exit_watcher(
        id: InstanceId,
        handle: &SupervisorHandle,
        backend: Arc<dyn HypervisorBackend>,
        backend_handle: BackendHandle,
    ) {
        let monitor_supervisor = handle.clone();
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let stream = backend.process_exit_stream(&backend_handle);
            let mut exit_events = Box::pin(stream);
            tracing::debug!(instance_id = %id, "process exit watcher subscribed");
            while exit_events.next().await.is_some() {
                tracing::debug!(
                    instance_id = %id,
                    state = ?monitor_supervisor.state(),
                    "qemu process exit event received"
                );
                match monitor_supervisor.state() {
                    state @ (InstanceState::Running
                    | InstanceState::Paused
                    | InstanceState::Starting) => {
                        let message = format!(
                            "QEMU process exited unexpectedly while in {:?} (see qemu.log)",
                            state
                        );
                        match monitor_supervisor
                            .transition(InstanceEvent::Fail(message))
                            .await
                        {
                            Ok(new_state) => tracing::debug!(
                                instance_id = %id,
                                ?new_state,
                                "supervised exit applied"
                            ),
                            Err(err) => tracing::error!(
                                instance_id = %id,
                                error = %err,
                                "failed to apply supervised exit transition"
                            ),
                        }
                    }
                    // A deliberate stop is in flight; the stop path
                    // owns the outcome and the events after this.
                    InstanceState::Stopping => break,
                    _ => break,
                }
            }
        });
    }

    pub async fn stop_instance(&self, id: InstanceId, graceful: bool) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;

        let state = handle.state();
        if let InstanceState::Error { message } = &state {
            return Err(DaemonError::InstanceAlreadyStopped(id, message.clone()));
        }

        let backend_handle = handle
            .backend_handle()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.to_string())))?;
        let backend = self.backend_for(handle.config().backend)?.clone();

        handle.transition(InstanceEvent::Stop).await?;

        let stop_result = backend.stop(&backend_handle, graceful).await;

        let result = match stop_result {
            Ok(()) => {
                handle.set_handle(None).await?;
                handle.transition(InstanceEvent::StopCompleted).await?;
                Ok(())
            }
            Err(backend_err) => {
                handle
                    .transition(InstanceEvent::Fail(backend_err.to_string()))
                    .await?;
                Err(DaemonError::Backend(backend_err))
            }
        };

        match &result {
            Ok(()) => tracing::info!(instance_id = %id, graceful, "instance stopped"),
            Err(err) => {
                tracing::error!(instance_id = %id, error = %err, "instance failed to stop")
            }
        }

        if result.is_ok() {
            spawn_compact_on_shutdown(id, handle.config().disk);
        }

        result
    }

    pub async fn pause_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let (backend, handle) = self.backend_and_handle(id).await?;
        let result = backend.pause(&handle).await.map_err(DaemonError::Backend);
        match &result {
            Ok(()) => {
                tracing::info!(instance_id = %id, "instance paused");
                self.apply_event_and_persist(id, InstanceEvent::Pause).await;
            }
            Err(err) => {
                tracing::error!(instance_id = %id, error = %err, "instance failed to pause")
            }
        }
        result
    }

    pub async fn resume_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let (backend, handle) = self.backend_and_handle(id).await?;
        let result = backend.resume(&handle).await.map_err(DaemonError::Backend);
        match &result {
            Ok(()) => {
                tracing::info!(instance_id = %id, "instance resumed");
                self.apply_event_and_persist(id, InstanceEvent::Resume)
                    .await;
            }
            Err(err) => {
                tracing::error!(instance_id = %id, error = %err, "instance failed to resume")
            }
        }
        result
    }

    pub async fn remove_instance(&self, id: InstanceId, purge: bool) -> Result<(), DaemonError> {
        if purge {
            let live_clones = self.find_live_clones(id).await?;
            if !live_clones.is_empty() {
                return Err(DaemonError::InstanceHasLiveClones(id, live_clones));
            }
        }

        let handle = self.handle_for(id).await?;
        let state = handle.state();

        let removable = matches!(
            state,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        );
        if !removable {
            return Err(DaemonError::InstanceNotRemovable(id, state));
        }

        let config = handle.config();
        let instance_dir = handle.instance_dir().to_path_buf();
        self.supervisors.write().await.remove(&id);
        handle.shutdown().await;

        if purge {
            super::types::purge_instance_files(id, &config, &instance_dir).await;
        } else {
            // Non-purge remove forgets the instance (registry entries) but
            // keeps the disk and firmware files. Without deleting the toml
            // the next daemon restart would resurrect a removed instance.
            super::types::remove_registry_entries(&config, &instance_dir).await;
        }

        tracing::info!(instance_id = %id, purge, "instance removed");
        Ok(())
    }

    /// Removes an instance entry regardless of registry health: healthy
    /// entries go through the normal FSM-guarded path, broken entries (no
    /// readable instance.toml) through the directory-based one.
    pub async fn remove_instance_entry(
        &self,
        id: InstanceId,
        purge: bool,
    ) -> Result<(), DaemonError> {
        if self.broken.read().await.contains_key(&id) {
            return self.remove_broken_instance(id, purge).await;
        }
        self.remove_instance(id, purge).await
    }

    /// Removes a broken registry entry (missing/invalid instance.toml) — the
    /// only operation that works on such entries. `purge` deletes the whole
    /// instance directory; without it the directory is left in place.
    pub async fn remove_broken_instance(
        &self,
        id: InstanceId,
        purge: bool,
    ) -> Result<(), DaemonError> {
        let reason = self
            .broken
            .write()
            .await
            .remove(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;
        let instance_dir = andler_core::paths::instances_root().join(id.to_string());
        if purge {
            if let Err(err) = tokio::fs::remove_dir_all(&instance_dir).await {
                tracing::error!(
                    instance_id = %id,
                    path = %instance_dir.display(),
                    error = %err,
                    "purge: failed to remove broken instance directory"
                );
            }
        }
        tracing::info!(instance_id = %id, purge, reason = %reason, "broken instance removed");
        Ok(())
    }

    /// Runs a command in the running guest via the guest agent (`andler
    /// exec`). Requires the instance Running with a responsive QGA.
    pub async fn guest_exec_command(
        &self,
        id: InstanceId,
        argv: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> Result<andler_core::GuestExecOutput, DaemonError> {
        if argv.is_empty() {
            return Err(DaemonError::InvalidConfigKey(
                "exec needs at least a program".to_string(),
            ));
        }
        let handle = self.handle_for(id).await?;
        let (state, backend_handle, backend_kind) = {
            (
                handle.state(),
                handle.backend_handle(),
                handle.config().backend,
            )
        };
        if !matches!(state, InstanceState::Running | InstanceState::Paused) {
            return Err(DaemonError::HotplugRequiresRunningInstance(id, state));
        }
        let backend_handle = backend_handle.ok_or_else(|| DaemonError::GuestAgentUnavailable {
            instance_id: id,
            message: "instance is running but has no backend handle".to_string(),
        })?;
        let backend = self.backend_for(backend_kind)?;
        let agent_unavailable = || DaemonError::GuestAgentUnavailable {
            instance_id: id,
            message:
                "guest agent is not available (is the VM booted and qemu-guest-agent running?)"
                    .to_string(),
        };
        // A missing/broken QGA socket surfaces as a connect error, not a
        // `false` — map it to the same actionable message instead of a raw
        // backend I/O error.
        let agent_available = backend
            .is_guest_agent_available(&backend_handle)
            .await
            .map_err(|_| agent_unavailable())?;
        if !agent_available {
            return Err(agent_unavailable());
        }
        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend
            .guest_exec_command(&backend_handle, &argv, timeout)
            .await
            .map_err(|_| agent_unavailable())
    }

    /// Runs one guest-side package command (install or remove) through the
    /// guest agent. Shared by the maintenance auto-start and the
    /// already-running online path so both execute identical steps.
    async fn run_package_exec(
        backend: &Arc<dyn HypervisorBackend>,
        backend_handle: &BackendHandle,
        package: &str,
        install: bool,
    ) -> Result<(), DaemonError> {
        let install_result: Result<(), BackendError> = if install {
            backend.guest_exec_install(backend_handle, package).await
        } else {
            backend.guest_exec_remove(backend_handle, package).await
        };
        install_result.map_err(DaemonError::Backend)
    }

    /// The operation id shared by the online path and the maintenance
    /// auto-start for one package, and the fallback join key when no
    /// idempotency token was supplied.
    fn guest_package_op_id(package: &str, install: bool) -> String {
        format!(
            "guest-{}-{package}",
            if install { "install" } else { "remove" }
        )
    }

    /// The in-flight-operation join key for a guest package request: the
    /// caller's idempotency token, or the operation's own id.
    fn guest_package_key(
        package: &str,
        install: bool,
        idempotency_token: Option<String>,
    ) -> Option<String> {
        idempotency_token.or_else(|| Some(Self::guest_package_op_id(package, install)))
    }

    /// Runs a guest-side package operation on an already-running VM as a
    /// supervisor operation — cancellable between the agent check and the
    /// command, progress visible on the event bus, same idempotency key as
    /// the maintenance auto-start (`guest-{action}-{package}`), so a
    /// repeated request joins the running one instead of double-installing.
    async fn online_package_op(
        &self,
        id: InstanceId,
        package: &str,
        install: bool,
        backend: &Arc<dyn HypervisorBackend>,
        backend_handle: BackendHandle,
        idempotency_token: Option<String>,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let action = if install { "installing" } else { "removing" };
        let op_id = Self::guest_package_op_id(package, install);
        let key = Self::guest_package_key(package, install, idempotency_token);
        let op_id_inner = op_id.clone();
        let kind = if install {
            OperationKind::GuestInstall
        } else {
            OperationKind::GuestRemove
        };
        let phases: Vec<(String, f32)> = vec![(action.to_string(), 1.0)];

        let run: OpRunner = {
            let backend = backend.clone();
            let package = package.to_string();
            Box::new(move |mut progress| {
                Box::pin(async move {
                    progress.enter_phase(action);
                    if progress.is_cancelled() {
                        let err = DaemonError::OperationCancelled(op_id_inner.clone());
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }
                    if let Err(err) =
                        Self::run_package_exec(&backend, &backend_handle, &package, install).await
                    {
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }
                    if progress.is_cancelled() {
                        let err = DaemonError::OperationCancelled(op_id_inner.clone());
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }
                    progress.set_progress(1.0);
                    progress.finish(Ok(()));
                    Ok(())
                })
            })
        };

        match handle
            .run_operation(
                Operation {
                    op_id: op_id.clone(),
                    instance_id: id,
                    kind,
                    phases: phases.clone(),
                    progress: 0.0,
                    state: OperationState::Queued,
                    error: None,
                },
                key,
                run,
            )
            .await?
        {
            OpAccept::Started { done } => done
                .await
                .map_err(|_| DaemonError::InstanceSupervisorGone(id))?,
            OpAccept::Joined { op_id: joined } => {
                tracing::warn!(
                    instance_id = %id,
                    joined = %joined,
                    "package operation joined an already-running one for the same package"
                );
                Ok(())
            }
        }
    }

    /// Boots a stopped instance headless for one package operation, then
    /// stops it again. Runs as a supervisor operation: the boot wait and the
    /// potentially long guest-side install are cancellable and visible on
    /// the event bus — never a blocking inline RPC.
    async fn auto_start_maintenance(
        &self,
        id: InstanceId,
        package: &str,
        install: bool,
        idempotency_token: Option<String>,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let cfg = handle.config();
        self.check_port_forward_conflicts(id, &cfg).await?;
        let backend = self.backend_for(cfg.backend)?.clone();

        let wait_secs = guest_agent_wait_secs();
        let action = if install { "install" } else { "remove" };
        let op_id = Self::guest_package_op_id(package, install);
        let key = Self::guest_package_key(package, install, idempotency_token);
        let op_id_inner = op_id.clone();
        let kind = if install {
            OperationKind::GuestInstall
        } else {
            OperationKind::GuestRemove
        };
        let phases: Vec<(String, f32)> = vec![
            ("starting VM".to_string(), 0.15),
            ("waiting for guest agent".to_string(), 0.45),
            (action.to_string(), 0.35),
            ("stopping VM".to_string(), 0.05),
        ];

        let run: OpRunner = {
            let handle = handle.clone();
            let backend = backend.clone();
            let cfg = cfg.clone();
            let package = package.to_string();
            Box::new(move |mut progress| {
                Box::pin(async move {
                    // Best-effort teardown shared by every failure path: the
                    // auto-started VM must never be left running behind a
                    // failed operation.
                    async fn stop_maintenance_vm(
                        progress: &mut OpProgress,
                        backend: &Arc<dyn HypervisorBackend>,
                        handle: &SupervisorHandle,
                        id: InstanceId,
                    ) {
                        progress.enter_phase("stopping VM");
                        let _ = handle.transition(InstanceEvent::Stop).await;
                        if let Some(bh) = handle.backend_handle() {
                            if let Err(err) = backend.stop(&bh, true).await {
                                tracing::warn!(instance_id = %id, error = %err, "failed to stop maintenance VM");
                                let _ = handle
                                    .transition(InstanceEvent::Fail(err.to_string()))
                                    .await;
                                return;
                            }
                        }
                        let _ = handle.set_handle(None).await;
                        let _ = handle.transition(InstanceEvent::StopCompleted).await;
                    }

                    progress.enter_phase("starting VM");
                    if let Err(err) = handle.transition(InstanceEvent::Start).await {
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }
                    let spawn_result = match validate_instance_files(&cfg) {
                        Ok(()) => backend.spawn(&cfg).await,
                        Err(e) => Err(e),
                    };
                    let backend_handle = match spawn_result {
                        Ok(bh) => bh,
                        Err(backend_err) => {
                            let _ = handle
                                .transition(InstanceEvent::Fail(backend_err.to_string()))
                                .await;
                            let err = DaemonError::Backend(backend_err);
                            progress.finish(Err(err.to_string()));
                            return Err(err);
                        }
                    };
                    Self::attach_process_exit_watcher(
                        id,
                        &handle,
                        backend.clone(),
                        backend_handle.clone(),
                    );
                    handle.set_handle(Some(backend_handle.clone())).await?;
                    if let Err(err) = handle.transition(InstanceEvent::StartCompleted).await {
                        stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }

                    progress.enter_phase("waiting for guest agent");
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
                    loop {
                        if progress.is_cancelled() {
                            stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                            let err = DaemonError::OperationCancelled(op_id_inner.clone());
                            progress.finish(Err(err.to_string()));
                            return Err(err);
                        }
                        match backend.is_guest_agent_available(&backend_handle).await {
                            Ok(true) => break,
                            Ok(false) => {}
                            Err(err) => {
                                tracing::warn!(instance_id = %id, error = %err, "guest agent probe failed during maintenance boot");
                            }
                        }
                        if std::time::Instant::now() >= deadline {
                            stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                            let err = DaemonError::GuestAgentUnavailable {
                                instance_id: id,
                                message: format!(
                                    "VM was auto-started for maintenance but the guest agent \
                                     did not respond within {wait_secs}s — the VM may not boot. \
                                     Retry with `--offline` to {action} `{package}` without \
                                     starting the VM (use `--offline` with the zero-root guestmount path)"
                                ),
                            };
                            progress.finish(Err(err.to_string()));
                            return Err(err);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }

                    progress.enter_phase(action);
                    if let Err(err) =
                        Self::run_package_exec(&backend, &backend_handle, &package, install).await
                    {
                        stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }
                    if progress.is_cancelled() {
                        stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                        let err = DaemonError::OperationCancelled(op_id_inner.clone());
                        progress.finish(Err(err.to_string()));
                        return Err(err);
                    }

                    stop_maintenance_vm(&mut progress, &backend, &handle, id).await;
                    tracing::info!(
                        instance_id = %id,
                        package = %package,
                        install,
                        "package operation via auto-started maintenance VM"
                    );
                    progress.set_progress(1.0);
                    progress.finish(Ok(()));
                    Ok(())
                })
            })
        };

        match handle
            .run_operation(
                Operation {
                    op_id: op_id.clone(),
                    instance_id: id,
                    kind,
                    phases: phases.clone(),
                    progress: 0.0,
                    state: OperationState::Queued,
                    error: None,
                },
                key,
                run,
            )
            .await?
        {
            OpAccept::Started { done } => done
                .await
                .map_err(|_| DaemonError::InstanceSupervisorGone(id))?,
            OpAccept::Joined { op_id: joined } => {
                tracing::warn!(
                    instance_id = %id,
                    joined = %joined,
                    "package operation joined an already-running one for the same package"
                );
                Ok(())
            }
        }
    }

    /// A request that arrives mid-transition (the maintenance auto-start is
    /// booting or stopping the VM) cannot start its own operation, but it
    /// must still follow the supervisor's accept rule: a same-key retry
    /// joins the in-flight operation and a different key is refused with
    /// `OperationAlreadyRunning`. Only when nothing is in flight does the
    /// state gate answer.
    async fn join_in_flight_guest_op(
        &self,
        handle: &SupervisorHandle,
        id: InstanceId,
        package: &str,
        install: bool,
        idempotency_token: Option<String>,
        state: &InstanceState,
    ) -> Result<(), DaemonError> {
        let key = Self::guest_package_key(package, install, idempotency_token);
        match handle.join_active(key).await? {
            Some(joined) => {
                tracing::warn!(
                    instance_id = %id,
                    joined = %joined,
                    "package operation joined an in-flight one"
                );
                Ok(())
            }
            None => Err(DaemonError::GuestAgentUnavailable {
                instance_id: id,
                message: format!(
                    "instance is in state {state:?}; must be Running/Paused (online) \
                     or Created/Stopped (offline)"
                ),
            }),
        }
    }

    pub async fn install_guest_agent(
        &self,
        id: InstanceId,
        package: String,
        offline: bool,
        idempotency_token: Option<String>,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let (state, disk_path, backend_handle, backend_kind) = {
            let config = handle.config();
            (
                handle.state(),
                config.disk.path.clone(),
                handle.backend_handle(),
                config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                if offline {
                    return Err(DaemonError::InvalidConfig(format!(
                        "`--offline` only applies to a stopped instance (currently {state:?}); \
                         `{package}` can be installed in the running VM without root on the host"
                    )));
                }
                let backend_handle =
                    backend_handle.ok_or_else(|| DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&backend_handle).await? {
                    let hint = match &state {
                        InstanceState::Paused => {
                            "VM is paused, so the guest agent cannot respond; \
                             resume the VM or stop it first to install `{package}` offline"
                        }
                        _ => {
                            "VM is running but guest agent is not available; \
                             stop the VM first to install `{package}` offline"
                        }
                    };
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: hint.to_string(),
                    });
                }
                self.online_package_op(
                    id,
                    &package,
                    true,
                    backend,
                    backend_handle,
                    idempotency_token.clone(),
                )
                .await?;
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package installed via online guest-exec"
                );
                Ok(())
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                match package.as_str() {
                    "libndk" | "libhoudini" => {
                        return Err(DaemonError::InvalidConfigKey(
                            "ARM translator packages must be installed via SwitchArmTranslator RPC, \
                             not InstallGuestAgent"
                                .to_string(),
                        ));
                    }
                    _ if offline => {
                        andler_disk::guest_tools::install_agent_offline(&disk_path, &package)
                            .await?;
                    }
                    _ => {
                        self.auto_start_maintenance(id, &package, true, idempotency_token)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package installed via offline guestmount"
                );
                Ok(())
            }
            other => {
                self.join_in_flight_guest_op(&handle, id, &package, true, idempotency_token, other)
                    .await
            }
        }
    }
    pub async fn remove_guest_agent(
        &self,
        id: InstanceId,
        package: String,
        offline: bool,
        idempotency_token: Option<String>,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let (state, disk_path, backend_handle, backend_kind) = {
            let config = handle.config();
            (
                handle.state(),
                config.disk.path.clone(),
                handle.backend_handle(),
                config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                if offline {
                    return Err(DaemonError::InvalidConfig(format!(
                        "`--offline` only applies to a stopped instance (currently {state:?}); \
                         `{package}` can be removed in the running VM without root on the host"
                    )));
                }
                let backend_handle =
                    backend_handle.ok_or_else(|| DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&backend_handle).await? {
                    let hint = match &state {
                        InstanceState::Paused => {
                            "VM is paused, so the guest agent cannot respond; \
                             resume the VM or stop it first to remove `{package}` offline"
                        }
                        _ => {
                            "VM is running but guest agent is not available; \
                             stop the VM first to remove `{package}` offline"
                        }
                    };
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: hint.to_string(),
                    });
                }
                self.online_package_op(
                    id,
                    &package,
                    false,
                    backend,
                    backend_handle,
                    idempotency_token.clone(),
                )
                .await?;
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package removed via online guest-exec"
                );
                Ok(())
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                match package.as_str() {
                    "libndk" | "libhoudini" => {
                        return Err(DaemonError::InvalidConfigKey(
                            "ARM translator packages must be removed via SwitchArmTranslator RPC, \
                             not RemoveGuestAgent"
                                .to_string(),
                        ));
                    }
                    _ if offline => {
                        andler_disk::guest_tools::remove_agent_offline(&disk_path, &package)
                            .await?;
                    }
                    _ => {
                        self.auto_start_maintenance(id, &package, false, idempotency_token)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package removed via offline guestmount"
                );
                Ok(())
            }
            other => {
                self.join_in_flight_guest_op(&handle, id, &package, false, idempotency_token, other)
                    .await
            }
        }
    }

    /// Applies a provision manifest (already expanded to `MutatorOp`s by the
    /// CLI) through the state-appropriate mutator: online via the guest
    /// agent when the VM is running, offline through the guestfs appliance
    /// when it is stopped. One apply call; the mutator batches.
    pub async fn guest_provision(
        &self,
        id: InstanceId,
        ops: Vec<MutatorOp>,
    ) -> Result<(), DaemonError> {
        if ops.is_empty() {
            return Err(DaemonError::InvalidConfig(
                "provision manifest contains no ops".to_string(),
            ));
        }
        let handle = self.handle_for(id).await?;
        let (state, disk_path, backend_handle, backend_kind) = {
            let config = handle.config();
            (
                handle.state(),
                config.disk.path.clone(),
                handle.backend_handle(),
                config.backend,
            )
        };

        match &state {
            InstanceState::Running => {
                let backend_handle =
                    backend_handle.ok_or_else(|| DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    })?;
                let backend = self.backend_for(backend_kind)?;
                if !backend.is_guest_agent_available(&backend_handle).await? {
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: format!(
                            "VM is running but the guest agent is not available; \
                             stop the VM first to provision `{id}` offline"
                        ),
                    });
                }
                let mutator = backend.guest_mutator(&backend_handle).await?;
                mutator.apply(&ops).await?;
            }
            InstanceState::Paused => {
                return Err(DaemonError::GuestAgentUnavailable {
                    instance_id: id,
                    message: format!(
                        "VM is paused, so the guest agent cannot respond; \
                         resume the VM or stop it first to provision `{id}`"
                    ),
                });
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }
                let mutator = andler_guestfs::GuestfsMutator::new(disk_path);
                mutator.apply(&ops).await?;
            }
            other => {
                return Err(DaemonError::GuestAgentUnavailable {
                    instance_id: id,
                    message: format!(
                        "instance is in state {other:?}; provision needs a stopped \
                         instance (offline appliance) or a running one with a guest agent"
                    ),
                });
            }
        }

        tracing::info!(
            instance_id = %id,
            ops = ops.len(),
            "provision manifest applied"
        );
        Ok(())
    }

    pub async fn list_guest_packages(
        &self,
        id: InstanceId,
    ) -> Result<Vec<(String, String, String)>, DaemonError> {
        let handle = self.handle_for(id).await?;
        let (state, disk_path, backend_handle, backend_kind, config_kind) = {
            let config = handle.config();
            (
                handle.state(),
                config.disk.path.clone(),
                handle.backend_handle(),
                config.backend,
                config.kind.clone(),
            )
        };

        let mut results = Vec::new();

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let backend_handle =
                    backend_handle.ok_or_else(|| DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    })?;

                let backend = self.backend_for(backend_kind)?;

                let packages = andler_disk::guest_tools::packages_for(&config_kind);
                for pkg in packages {
                    let mut installed = false;
                    for binary in pkg.binary_checks {
                        if backend
                            .guest_check_binary_installed(&backend_handle, binary)
                            .await
                            .unwrap_or(false)
                        {
                            installed = true;
                            break;
                        }
                    }
                    results.push((
                        pkg.name.to_string(),
                        pkg.description.to_string(),
                        if installed {
                            "installed"
                        } else {
                            "not_installed"
                        }
                        .to_string(),
                    ));
                }
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                let package_status = andler_disk::guest_tools::check_packages_offline_with_disk(
                    &disk_path,
                    &config_kind,
                )?;
                for (pkg, status) in package_status {
                    results.push((
                        pkg.name.to_string(),
                        pkg.description.to_string(),
                        match status {
                            andler_disk::guest_tools::PackageStatus::Installed => "installed",
                            andler_disk::guest_tools::PackageStatus::NotInstalled => {
                                "not_installed"
                            }
                            andler_disk::guest_tools::PackageStatus::Unknown => "unknown",
                        }
                        .to_string(),
                    ));
                }
            }
            other => {
                return Err(DaemonError::GuestAgentUnavailable {
                    instance_id: id,
                    message: format!("instance is in state {other:?}; cannot list packages"),
                });
            }
        }

        Ok(results)
    }

    pub async fn switch_android_boot_mode(
        &self,
        id: InstanceId,
        mode: andler_core::AndroidBootMode,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let (overlay_path, state, kind, instance_dir) = {
            let config = handle.config();
            (
                config.disk.path.clone(),
                handle.state(),
                config.kind.clone(),
                handle.instance_dir().to_path_buf(),
            )
        };

        match &kind {
            InstanceKind::AndroidVm { .. } => {}
            _ => return Err(DaemonError::NotAndroid(id)),
        }

        if !state.is_disk_idle() {
            return Err(DaemonError::InstanceMustBeStopped(id, state));
        }

        andler_disk::boot_mode::switch_boot_mode_with(
            &andler_guestfs::GuestfsMutator::new(overlay_path.clone()),
            mode,
        )
        .await?;

        // The instance.toml is the source of truth for the effective
        // profile record the new boot mode so `get` and `connect`
        // never need an offline disk mount. Both the file and the in-memory
        // config are updated — a get right after switch must see the new
        // value without a daemon restart.
        let mut cfg = handle.config().clone();
        if let InstanceKind::AndroidVm { android_profile } = &mut cfg.kind {
            android_profile.boot_mode = mode;
        }
        write_instance_toml(&instance_dir, &cfg).await;
        handle.set_config(cfg, None).await?;

        tracing::info!(instance_id = %id, mode = ?mode, "android boot mode switched successfully");
        Ok(())
    }

    pub async fn get_android_boot_mode(
        &self,
        id: InstanceId,
    ) -> Result<andler_core::AndroidBootMode, DaemonError> {
        let handle = self.handle_for(id).await?;
        let kind = handle.config().kind.clone();

        match &kind {
            InstanceKind::AndroidVm { android_profile } => Ok(android_profile.boot_mode),
            _ => Err(DaemonError::NotAndroid(id)),
        }
    }

    pub async fn switch_arm_translator(
        &self,
        id: InstanceId,
        translator: andler_core::android_profile::ArmTranslator,
        translator_dir: Option<PathBuf>,
    ) -> Result<andler_disk::arm_translator::TranslatorSwitch, DaemonError> {
        let handle = self.handle_for(id).await?;
        let (overlay_path, state, kind) = {
            let config = handle.config();
            (
                config.disk.path.clone(),
                handle.state(),
                config.kind.clone(),
            )
        };

        match &kind {
            andler_core::config::InstanceKind::AndroidVm { .. } => {}
            _ => return Err(DaemonError::NotAndroid(id)),
        }

        if !state.is_disk_idle() {
            return Err(DaemonError::InstanceMustBeStopped(id, state));
        }

        let android_version = match &kind {
            andler_core::config::InstanceKind::AndroidVm { android_profile } => {
                android_profile.android_version.to_string()
            }
            _ => "13".to_string(),
        };

        // A switch opens a libguestfs session, downloads the payload on a cold
        // cache (~18 MiB) and runs three appliance batches, so it takes
        // minutes. It runs as a supervisor operation with named stages: as an
        // inline call the CLI could only show a spinner that said nothing for
        // the whole duration.
        use andler_disk::arm_translator::{TranslatorProgress, TranslatorStage};

        /// Share of the operation the download occupies; the byte reporter
        /// maps its fraction onto exactly this slice.
        const DOWNLOAD_PHASE_WEIGHT: f32 = 0.35;

        let op_id = format!("op-{}", uuid::Uuid::new_v4());
        let phases: Vec<(String, f32)> = vec![
            (
                TranslatorStage::Downloading.label().to_string(),
                DOWNLOAD_PHASE_WEIGHT,
            ),
            (TranslatorStage::OpeningDisk.label().to_string(), 0.30),
            (TranslatorStage::Staging.label().to_string(), 0.25),
            (TranslatorStage::Cleanup.label().to_string(), 0.05),
            (TranslatorStage::Properties.label().to_string(), 0.05),
        ];
        // Same translator + same source dir is the same work, so a retry joins
        // the running operation instead of starting a second appliance session
        // on the same disk.
        let key = Some(match &translator_dir {
            Some(dir) => format!("arm-translator:{translator}:{}", dir.display()),
            None => format!("arm-translator:{translator}"),
        });

        let outcome: Arc<std::sync::Mutex<Option<andler_disk::arm_translator::TranslatorSwitch>>> =
            Arc::new(std::sync::Mutex::new(None));
        let outcome_for_run = outcome.clone();
        let op_id_for_cancel = op_id.clone();

        let run: OpRunner = Box::new(move |mut progress| {
            Box::pin(async move {
                let (progress_tx, mut watch) = TranslatorProgress::channel();
                let mutator = andler_guestfs::GuestfsMutator::new(overlay_path.clone());
                let switch = andler_disk::arm_translator::switch_translator_with(
                    &mutator,
                    translator,
                    translator_dir,
                    &android_version,
                    Some(&progress_tx),
                );
                tokio::pin!(switch);

                let result = loop {
                    tokio::select! {
                        result = &mut switch => break result,
                        changed = watch.stage.changed() => {
                            if changed.is_err() {
                                continue;
                            }
                            let stage = *watch.stage.borrow();
                            // One log line per stage: the daemon log is the
                            // other place an operator looks while a guest
                            // operation runs.
                            tracing::info!(
                                instance_id = %id,
                                stage = stage.label(),
                                "translator switch"
                            );
                            progress.enter_phase(stage.label());
                            if progress.is_cancelled() {
                                let err = DaemonError::OperationCancelled(op_id_for_cancel.clone());
                                progress.finish(Err(err.to_string()));
                                return Err(err);
                            }
                        }
                        changed = watch.download.changed() => {
                            if changed.is_err() {
                                continue;
                            }
                            let (fetched, total) = *watch.download.borrow();
                            if total > 0 {
                                // The download is the first phase, so its
                                // share of the bar is the phase weight: a
                                // percentage that moves during the transfer
                                // is what the reader needs to tell a slow
                                // mirror from a stuck appliance.
                                let share = fetched as f32 / total as f32;
                                progress.set_progress(DOWNLOAD_PHASE_WEIGHT * share);
                            }
                        }
                    }
                };

                match result {
                    Ok(switch) => {
                        *outcome_for_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(switch);
                        progress.set_progress(1.0);
                        progress.finish(Ok(()));
                        Ok(())
                    }
                    Err(err) => {
                        let err = DaemonError::Disk(err);
                        progress.finish(Err(err.to_string()));
                        Err(err)
                    }
                }
            })
        });

        match handle
            .run_operation(
                Operation {
                    op_id: op_id.clone(),
                    instance_id: id,
                    kind: OperationKind::GuestInstall,
                    phases: phases.clone(),
                    progress: 0.0,
                    state: OperationState::Queued,
                    error: None,
                },
                key,
                run,
            )
            .await?
        {
            OpAccept::Started { done } => {
                // Two layers: the supervisor channel's own error, then the
                // operation's result.
                done.await
                    .map_err(|_| DaemonError::InstanceSupervisorGone(id))??;
            }
            OpAccept::Joined { op_id: joined } => {
                tracing::warn!(
                    instance_id = %id,
                    joined = %joined,
                    translator = ?translator,
                    "translator switch joined an already-running operation"
                );
            }
        }

        let switch = outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(andler_disk::arm_translator::TranslatorSwitch::AlreadyInstalled);

        tracing::info!(
            instance_id = %id,
            translator = ?translator,
            "ARM translator switched successfully"
        );
        Ok(switch)
    }

    /// `config set`: read-modify-write one key on instance.toml (the source
    /// of truth), apply it to the running guest when the key is live, then
    /// push the result into the supervisor's memory. Unknown/immutable keys
    /// and invalid values are rejected with a reason; manual file edits on
    /// other keys survive because only the target key is rewritten.
    pub async fn set_instance_config(
        &self,
        id: InstanceId,
        key: &str,
        value: &str,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let snapshot = handle.reload_config().await?;
        if let Some(message) = &snapshot.file_error {
            let path = instance_dir_of(&snapshot.config).join("instance.toml");
            return Err(DaemonError::ConfigFileInvalid {
                path,
                message: message.clone(),
            });
        }

        let mut cfg = snapshot.config;
        let instance_dir = instance_dir_of(&cfg);
        let state = handle.state();

        // Live keys apply to the running guest immediately.
        let mut applied_live_resolution = None;
        if key == "display.resolution" {
            let resolution = parse_resolution(value)?;
            if matches!(state, InstanceState::Running | InstanceState::Paused) {
                let (backend, backend_handle) = self.backend_and_handle(id).await?;
                backend
                    .set_guest_display_resolution(&backend_handle, &resolution, cfg.kind.clone())
                    .await?;
                applied_live_resolution = Some(resolution);
            }
        }

        // The ARM translator is applied to the disk image, which requires a
        // stopped VM; the config file update then records the new value.
        if key == "kind.android_profile.arm_translator" {
            if !state.is_disk_idle() {
                return Err(DaemonError::InstanceMustBeStopped(id, state));
            }
            let translator: andler_core::android_profile::ArmTranslator = value
                .parse()
                .map_err(|_| DaemonError::InvalidConfigKey(key.to_string()))?;
            self.switch_arm_translator(id, translator, None).await?;
        }

        andler_core::config::set_key(&mut cfg, key, value)
            .map_err(DaemonError::from_config_key_error)?;
        cfg.validate().map_err(DaemonError::InvalidConfig)?;

        super::types::write_instance_toml(&instance_dir, &cfg).await;
        handle.set_config(cfg, applied_live_resolution).await?;

        tracing::info!(instance_id = %id, key, value, "config set");
        Ok(())
    }
}

fn instance_dir_of(cfg: &InstanceConfig) -> std::path::PathBuf {
    cfg.disk
        .path
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn parse_resolution(value: &str) -> Result<Resolution, DaemonError> {
    let (width, height) = value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .filter(|(w, h)| *w > 0 && *h > 0)
        .ok_or_else(|| {
            DaemonError::InvalidConfigKey(
                "display.resolution expects WxH, e.g. 1920x1080".to_string(),
            )
        })?;
    Ok(Resolution::new(width, height))
}

fn spawn_compact_on_shutdown(id: InstanceId, disk: andler_core::DiskConfig) {
    if !disk.compact_on_shutdown {
        return;
    }
    if disk.format != DiskFormat::Qcow2 {
        tracing::debug!(
            instance_id = %id,
            format = ?disk.format,
            "compact_on_shutdown is enabled but disk format is not qcow2 — skipping"
        );
        return;
    }

    let path = disk.path.clone();
    tokio::spawn(async move {
        tracing::info!(
            instance_id = %id,
            path = %path.display(),
            "compact_on_shutdown: starting automatic disk compaction"
        );
        match andler_disk::qcow2::compact(&path).await {
            Ok(()) => {
                tracing::info!(
                    instance_id = %id,
                    path = %path.display(),
                    "compact_on_shutdown: disk compaction finished"
                );
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %id,
                    path = %path.display(),
                    error = %err,
                    "compact_on_shutdown: automatic disk compaction failed"
                );
            }
        }
    });
}

/// Start-sequence body shared between the `StartInstance` RPC and the
/// daemon.s deferred `autostart` task. The RPC and the task
/// must use the same path so supervisor/QMP/exit-watcher guarantees
/// stay uniform; the only difference is how `&self` is obtained.
pub(super) async fn do_start_instance(
    start_lock: &tokio::sync::Mutex<()>,
    supervisors: &std::sync::Arc<tokio::sync::RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    backends: &HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    id: InstanceId,
) -> Result<(), DaemonError> {
    let handle = {
        let map = supervisors.read().await;
        map.get(&id)
            .cloned()
            .ok_or(DaemonError::InstanceNotFound(id))?
    };
    let cfg = handle.config();

    // Hold the global start lock across the check→resource-claim critical
    // section so concurrent starts serialize: once this instance reaches
    // Starting it owns its disk/host-port/pinned-CPU/guest-RAM, and a
    // parallel start's per-start gates see it as active and refuse. Without
    // this, two instances still in Created and started concurrently both
    // pass the pairwise checks (the peer is only Created, never counted)
    // and both spawn against the same disk or host port — the §H
    // multi-instance TOCTOU race. Released before the slow spawn below.
    let _guard = start_lock.lock().await;
    {
        // Checked before the Start transition so a refused start leaves the
        // instance in Created, not stuck in Starting. Same port-conflict
        // check as the RPC path (see check_port_forward_conflicts).
        check_port_forward_conflicts_impl(supervisors, id, &cfg).await?;
        // Same for the disk: QEMU takes an exclusive write lock on the disk
        // file, so a second instance pointing at the same disk would fail
        // with an opaque lock error at spawn — refuse it up front.
        check_disk_conflicts_impl(supervisors, id, &cfg).await?;
        // And for pinned CPUs: two instances pinned to overlapping host CPUs
        // would silently contend for the same cores, defeating the pin.
        check_cpu_affinity_conflicts_impl(supervisors, id, &cfg).await?;
        // The pinned set must also be valid (no CPU beyond the host's count) —
        // a bad set fails fast instead of surfacing as a raw taskset error at spawn.
        validate_cpu_affinity(&cfg)?;
        // And the last shared-resource gate: the sum of guest RAM across every
        // running instance (plus this one) must not exceed the host's physical RAM.
        check_memory_overcommit_impl(supervisors, id, &cfg).await?;
        handle.transition(InstanceEvent::Start).await?;
    }

    let backend = backends
        .get(&cfg.backend)
        .ok_or(DaemonError::NoBackendRegistered(cfg.backend))?
        .clone();

    let spawn_result = match validate_instance_files(&cfg) {
        Ok(()) => backend.spawn(&cfg).await,
        Err(e) => Err(e),
    };

    match spawn_result {
        Ok(backend_handle) => {
            // Subscribe as early as possible so a crash between spawn
            // and StartCompleted is still caught; the watcher outlives
            // set_handle below either way.
            Daemon::attach_process_exit_watcher(
                id,
                &handle,
                backend.clone(),
                backend_handle.clone(),
            );
            handle.set_handle(Some(backend_handle)).await?;
            handle.transition(InstanceEvent::StartCompleted).await?;
            tracing::info!(instance_id = %id, "instance started");
            Ok(())
        }
        Err(backend_err) => {
            handle
                .transition(InstanceEvent::Fail(backend_err.to_string()))
                .await?;
            tracing::error!(instance_id = %id, error = %backend_err, "instance failed to start");
            Err(DaemonError::Backend(backend_err))
        }
    }
}

/// the two paths can never drift.
async fn check_port_forward_conflicts_impl(
    supervisors: &std::sync::Arc<tokio::sync::RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    id: InstanceId,
    cfg: &InstanceConfig,
) -> Result<(), DaemonError> {
    let wanted: Vec<u16> = std::iter::once(&cfg.network)
        .chain(cfg.extra_networks.iter())
        .flat_map(|net| net.port_forwards.iter().map(|f| f.host_port))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }

    let map = supervisors.read().await;
    for (other_id, other) in map.iter() {
        if *other_id == id {
            continue;
        }
        let state = other.state();
        if !matches!(
            state,
            InstanceState::Running | InstanceState::Paused | InstanceState::Starting
        ) {
            continue;
        }
        let other_cfg = other.config();
        let held: Vec<u16> = std::iter::once(&other_cfg.network)
            .chain(other_cfg.extra_networks.iter())
            .flat_map(|net| net.port_forwards.iter().map(|f| f.host_port))
            .collect();
        for port in &wanted {
            if held.contains(port) {
                return Err(DaemonError::PortForwardConflict {
                    port: *port,
                    instance: id,
                    held_by: *other_id,
                });
            }
        }
    }
    Ok(())
}

/// Shared disk-path conflict check used by both the `StartInstance` RPC
/// and the deferred `autostart` task, so the two paths can never drift
/// QEMU takes an exclusive write lock on the disk file; two
/// instances pointing at the same disk (primary or extra) would
/// otherwise fail with an opaque lock error at best and corrupt the disk
/// at worst. Shared base images are fine (read-only backing files), so
/// only the top-level disk paths are compared.
async fn check_disk_conflicts_impl(
    supervisors: &std::sync::Arc<tokio::sync::RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    id: InstanceId,
    cfg: &InstanceConfig,
) -> Result<(), DaemonError> {
    let wanted: Vec<std::path::PathBuf> = std::iter::once(&cfg.disk.path)
        .chain(cfg.extra_disks.iter().map(|d| &d.path))
        .cloned()
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }

    let map = supervisors.read().await;
    for (other_id, other) in map.iter() {
        if *other_id == id {
            continue;
        }
        let state = other.state();
        if !matches!(
            state,
            InstanceState::Running | InstanceState::Paused | InstanceState::Starting
        ) {
            continue;
        }
        let other_cfg = other.config();
        let held: Vec<std::path::PathBuf> = std::iter::once(&other_cfg.disk.path)
            .chain(other_cfg.extra_disks.iter().map(|d| &d.path))
            .cloned()
            .collect();
        for path in &wanted {
            if held.contains(path) {
                return Err(DaemonError::DiskInUse {
                    path: path.clone(),
                    instance: id,
                    held_by: *other_id,
                });
            }
        }
    }
    Ok(())
}

/// Shared pinned-CPU conflict check used by both the `StartInstance` RPC
/// and the deferred `autostart` task. Only instances with an explicit
/// `cpu.affinity` participate: two pinned instances sharing a host CPU
/// would silently contend for that core, so the second start is refused.
/// Unpinned instances are left to the scheduler and never conflict.
async fn check_cpu_affinity_conflicts_impl(
    supervisors: &std::sync::Arc<tokio::sync::RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    id: InstanceId,
    cfg: &InstanceConfig,
) -> Result<(), DaemonError> {
    let Some(wanted) = &cfg.cpu.affinity else {
        return Ok(());
    };
    if wanted.is_empty() {
        return Ok(());
    }

    let map = supervisors.read().await;
    for (other_id, other) in map.iter() {
        if *other_id == id {
            continue;
        }
        let state = other.state();
        if !matches!(
            state,
            InstanceState::Running | InstanceState::Paused | InstanceState::Starting
        ) {
            continue;
        }
        let Some(held) = &other.config().cpu.affinity else {
            continue;
        };
        for cpu in wanted {
            if held.contains(cpu) {
                return Err(DaemonError::CpuAffinityConflict {
                    cpu: *cpu,
                    instance: id,
                    held_by: *other_id,
                });
            }
        }
    }
    Ok(())
}
/// Shared multi-instance memory gate used by both the `StartInstance` RPC and
/// the deferred `autostart` task: sum the guest RAM of every Running/Paused/
/// Starting instance plus the one being started, and refuse if the sum would
/// exceed the host's total RAM. This is the §H "Multi-instance resources"
/// remainder — the port/disk/cpu conflict gates already handle the other
/// shared-resource races; memory is the last one. KSM is enabled by default
/// but we do not credit it: shared pages are unpredictable, so the gate stays
/// conservative and never silently over-commits.
async fn check_memory_overcommit_impl(
    supervisors: &std::sync::Arc<tokio::sync::RwLock<HashMap<InstanceId, SupervisorHandle>>>,
    id: InstanceId,
    cfg: &InstanceConfig,
) -> Result<(), DaemonError> {
    // The operator may override the host-total used for the comparison with
    // `memory.hostmem_bytes` (the hard RAM the guest is allowed to touch);
    // otherwise we compare against the host's physical RAM.
    let host_total = read_host_total_ram().unwrap_or(cfg.memory.size_bytes);

    let map = supervisors.read().await;
    let mut used_bytes = 0u64;
    let mut running_count = 0usize;
    for (other_id, other) in map.iter() {
        if *other_id == id {
            continue;
        }
        let state = other.state();
        if !matches!(
            state,
            InstanceState::Running | InstanceState::Paused | InstanceState::Starting
        ) {
            continue;
        }
        used_bytes += other.config().memory.size_bytes;
        running_count += 1;
    }

    // The new instance's RAM is always part of the sum, even if it is the
    // only instance (a fresh host with one 16 GiB VM must still fail if the
    // host has only 8 GiB).
    let requested_bytes = used_bytes + cfg.memory.size_bytes;

    if requested_bytes > host_total {
        return Err(DaemonError::MemoryOvercommit {
            requested_bytes,
            host_bytes: host_total,
            running_count,
            used_bytes,
        });
    }

    Ok(())
}

/// Host physical RAM in bytes, read from `/proc/meminfo` (MemTotal) on Linux.
/// Returns `None` when the file is unreadable or the field is missing — the
/// caller then falls back to the instance's own size, which still catches the
/// degenerate "start one VM larger than itself" case without panicking.
fn read_host_total_ram() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in raw.lines() {
        let (key, value) = line.split_once(':')?;
        if key.trim() == "MemTotal" {
            // "<value> kB" — the value is in kilobytes. The field is
            // whitespace-padded, so trim before splitting off the unit.
            let (num, unit) = value.trim().split_once(' ')?;
            if unit.trim() == "kB" {
                if let Ok(kb) = num.trim().parse::<u64>() {
                    return Some(kb.saturating_mul(1024));
                }
            }
        }
    }
    None
}

/// Rejects a `cpu.affinity` set that references host CPUs the QEMU process
/// could never be pinned to (index >= the host's available parallelism).
/// Checked before the Start transition so a bad set fails fast instead of
/// surfacing as a raw taskset error at spawn.
fn validate_cpu_affinity(cfg: &InstanceConfig) -> Result<(), DaemonError> {
    let Some(affinity) = &cfg.cpu.affinity else {
        return Ok(());
    };
    let host_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    for cpu in affinity {
        if *cpu >= host_cpus {
            return Err(DaemonError::InvalidConfig(format!(
                "cpu.affinity references host CPU {cpu}, but this host exposes {host_cpus} \
                 (0-{}); edit the affinity set in instance.toml",
                host_cpus.saturating_sub(1)
            )));
        }
    }
    Ok(())
}

/// How long a maintenance auto-start waits for the guest agent before
/// giving up (default 120s; overridable for tests with
/// ANDLERD_GUEST_AGENT_WAIT_SECS, floor 5s).
fn guest_agent_wait_secs() -> u64 {
    match std::env::var("ANDLERD_GUEST_AGENT_WAIT_SECS") {
        Ok(v) => v.parse::<u64>().unwrap_or(120).max(5),
        Err(_) => 120,
    }
}
