use std::path::PathBuf;
use std::sync::Arc;

use super::error::DaemonError;
use super::spawn_supervisor;
use super::types::{write_instance_toml, InstanceDirGuard};
use super::Daemon;
use super::SupervisorHandle;
use andler_core::{
    BackendError, BackendHandle, DaemonEvent, DiskFormat, EventKind, EventLogLevel,
    HypervisorBackend, InstanceConfig, InstanceEvent, InstanceId, InstanceKind, InstanceState,
    Resolution, INSTANCE_ID_HEX_LEN,
};

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
        cfg.validate().map_err(DaemonError::InvalidConfig)?;
        self.backend_for(cfg.backend)?;

        let id = cfg.id;
        let handle = spawn_supervisor(
            id,
            instance_dir,
            cfg.clone(),
            InstanceState::Created,
            None,
            self.event_sender(),
        );
        self.supervisors.write().await.insert(id, handle);

        let _ = self.event_sender().send(DaemonEvent {
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
        profile: andler_core::AndroidProfile,
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
        let handle = self.handle_for(id).await?;
        let cfg = handle.config();
        // Checked before the Start transition so a refused start leaves the
        // instance in Created, not stuck in Starting.
        self.check_port_forward_conflicts(id, &cfg).await?;
        handle.transition(InstanceEvent::Start).await?;

        let backend = self.backend_for(cfg.backend)?.clone();

        let spawn_result = match validate_instance_files(&cfg) {
            Ok(()) => backend.spawn(&cfg).await,
            Err(e) => Err(e),
        };

        match spawn_result {
            Ok(backend_handle) => {
                // Subscribe as early as possible so a crash between spawn
                // and StartCompleted is still caught; the watcher outlives
                // set_handle below either way.
                Self::attach_process_exit_watcher(
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

    /// Refuses to start an instance whose `network.port_forwards` host
    /// ports are already forwarded by another instance that is running
    /// (or starting) right now. Two QEMU slirp netdevs bound to the same
    /// host port would otherwise fail with an opaque QEMU error at best
    /// and silently misroute traffic at worst — the host port is a shared
    /// resource across instances (PLAN §12.B).
    async fn check_port_forward_conflicts(
        &self,
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

        let supervisors = self.supervisors.read().await;
        for (other_id, other) in supervisors.iter() {
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

    pub async fn install_guest_agent(
        &self,
        id: InstanceId,
        package: String,
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

                backend
                    .guest_exec_install(&backend_handle, &package)
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
                    _ => {
                        andler_disk::guest_tools::install_agent_offline(&disk_path, &package)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package installed via offline qemu-nbd"
                );
                Ok(())
            }
            other => Err(DaemonError::GuestAgentUnavailable {
                instance_id: id,
                message: format!(
                    "instance is in state {other:?}; must be Running/Paused (online) \
                     or Created/Stopped (offline)"
                ),
            }),
        }
    }

    pub async fn remove_guest_agent(
        &self,
        id: InstanceId,
        package: String,
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

                backend.guest_exec_remove(&backend_handle, &package).await?;
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
                    _ => {
                        andler_disk::guest_tools::remove_agent_offline(&disk_path, &package)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id,
                    package = %package,
                    "package removed via offline qemu-nbd"
                );
                Ok(())
            }
            other => Err(DaemonError::GuestAgentUnavailable {
                instance_id: id,
                message: format!(
                    "instance is in state {other:?}; must be Running/Paused (online) \
                     or Created/Stopped (offline)"
                ),
            }),
        }
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

                let is_android = matches!(&config_kind, InstanceKind::AndroidVm { .. });
                let packages = if is_android {
                    andler_disk::guest_tools::ANDROID_PACKAGES
                } else {
                    andler_disk::guest_tools::KNOWN_PACKAGES
                };
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

                let is_android = matches!(&config_kind, InstanceKind::AndroidVm { .. });
                let package_status = if is_android {
                    andler_disk::guest_tools::check_android_packages_offline_with_disk(&disk_path)?
                } else {
                    andler_disk::guest_tools::check_all_packages_offline_with_disk(&disk_path)?
                };
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
        // profile (P31): record the new boot mode so `get` and `connect`
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
    ) -> Result<(), DaemonError> {
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

        andler_disk::arm_translator::switch_translator(
            &overlay_path,
            translator,
            translator_dir,
            &android_version,
        )
        .await?;

        tracing::info!(
            instance_id = %id,
            translator = ?translator,
            "ARM translator switched successfully"
        );
        Ok(())
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
