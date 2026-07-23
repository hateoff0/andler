use std::path::PathBuf;

use super::Daemon;
use super::error::DaemonError;
use super::types::{InstanceDirGuard, InstanceRecord, write_instance_toml};
use andler_core::{
    BackendError, DiskFormat, InstanceConfig, InstanceEvent, InstanceId,
    InstanceKind, InstanceState,
};

impl Daemon {
    pub async fn resolve_instance_id(&self, raw: &str) -> Result<InstanceId, DaemonError> {
        if raw.is_empty() {
            return Err(DaemonError::EmptyInstanceRef);
        }

        if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
            return Ok(InstanceId(uuid));
        }

        if !raw.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(DaemonError::MalformedInstanceRef(raw.to_string()));
        }

        let needle = raw.to_ascii_lowercase();
        let instances = self.instances.read().await;
        let matches: Vec<InstanceId> = instances
            .keys()
            .filter(|id| id.0.to_string().starts_with(&needle))
            .copied()
            .collect();

        match matches.len() {
            0 => Err(DaemonError::InstanceRefNotFound(raw.to_string())),
            1 => Ok(matches[0]),
            _ => Err(DaemonError::AmbiguousInstanceId {
                prefix: raw.to_string(),
                candidates: matches,
            }),
        }
    }

    pub async fn create_instance(&self, cfg: InstanceConfig) -> Result<InstanceId, DaemonError> {
        self.backend_for(cfg.backend)?;

        let id = cfg.id;
        {
            let mut instances = self.instances.write().await;
            instances.insert(
                id,
                InstanceRecord {
                    config: cfg.clone(),
                    state: InstanceState::Created,
                    handle: None,
                },
            );
        }
        self.persist_new_instance(&cfg, &InstanceState::Created).await;

        Ok(id)
    }

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
        let instance_dir = instances_root.join(id.0.to_string());
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

        let cfg = profile.resolve(instance_name, disk, ovmf_vars_path);

        let registered_id = self.create_instance(cfg).await?;

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
        let instance_dir = instances_root.join(id.0.to_string());
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

        let registered_id = self.create_instance(cfg).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    pub async fn start_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let starting_state = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get_mut(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            record.state = record.state.clone().apply(InstanceEvent::Start)?;
            record.state.clone()
        };
        self.persist_state(id, &starting_state).await;

        let (backend, cfg) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                self.backend_for(record.config.backend)?.clone(),
                record.config.clone(),
            )
        };

        let spawn_result = backend.spawn(&cfg).await;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let result = match spawn_result {
            Ok(handle) => {
                record.handle = Some(handle);
                record.state = record.state.clone().apply(InstanceEvent::StartCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        };
        let final_state = record.state.clone();
        drop(instances);

        self.persist_state(id, &final_state).await;
        result
    }

    pub async fn stop_instance(&self, id: InstanceId, graceful: bool) -> Result<(), DaemonError> {
        let (handle, backend, stopping_state) = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get_mut(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let handle = record.handle.clone().ok_or_else(|| {
                DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string()))
            })?;

            record.state = record.state.clone().apply(InstanceEvent::Stop)?;
            let backend = self.backend_for(record.config.backend)?.clone();

            (handle, backend, record.state.clone())
        };
        self.persist_state(id, &stopping_state).await;

        let stop_result = backend.stop(&handle, graceful).await;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let result = match stop_result {
            Ok(()) => {
                record.handle = None;
                record.state = record.state.clone().apply(InstanceEvent::StopCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        };
        let final_state = record.state.clone();
        let disk = record.config.disk.clone();
        drop(instances);

        self.persist_state(id, &final_state).await;

        if result.is_ok() {
            spawn_compact_on_shutdown(id, disk);
        }

        result
    }

    pub async fn pause_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

        backend.pause(&handle).await.map_err(DaemonError::Backend)
    }

    pub async fn resume_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let handle = record.handle.clone().ok_or_else(|| {
                DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string()))
            })?;
            let backend = self.backend_for(record.config.backend)?.clone();

            (backend, handle)
        };

        backend.resume(&handle).await.map_err(DaemonError::Backend)
    }

    pub async fn remove_instance(&self, id: InstanceId, purge: bool) -> Result<(), DaemonError> {
        if purge {
            let live_clones = self.find_live_clones(id).await?;
            if !live_clones.is_empty() {
                return Err(DaemonError::InstanceHasLiveClones(id, live_clones));
            }
        }

        let config = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let removable = matches!(
                record.state,
                InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
            );
            if !removable {
                return Err(DaemonError::InstanceNotRemovable(id, record.state.clone()));
            }

            instances.remove(&id).map(|record| record.config)
        };

        if let Some(store) = &self.store {
            if let Err(err) = store.delete_instance(id).await {
                tracing::error!(
                    instance_id = %id.0,
                    error = %err,
                    "failed to delete instance from store after in-memory removal"
                );
            }
        }

        if purge {
            if let Some(config) = config {
                super::types::purge_instance_files(id, &config).await;
            }
        }

        Ok(())
    }

    pub async fn install_guest_agent(
        &self,
        id: InstanceId,
        package: String,
    ) -> Result<(), DaemonError> {
        let (state, disk_path, handle, backend_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&handle).await? {
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: format!(
                            "VM is running but guest agent is not available; \
                             stop the VM first to install `{package}` offline"
                        ),
                    });
                }

                backend.guest_exec_install(&handle, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
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
                        return Err(DaemonError::InvalidConfigKey(format!(
                            "ARM translator packages must be installed via SwitchArmTranslator RPC, \
                             not InstallGuestAgent"
                        )));
                    }
                    _ => {
                        andler_disk::guest_tools::install_agent_offline(&disk_path, &package)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id.0,
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
        let (state, disk_path, handle, backend_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&handle).await? {
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: format!(
                            "VM is running but guest agent is not available; \
                             stop the VM first to remove `{package}` offline"
                        ),
                    });
                }

                backend.guest_exec_remove(&handle, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
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
                        return Err(DaemonError::InvalidConfigKey(format!(
                            "ARM translator packages must be removed via SwitchArmTranslator RPC, \
                             not RemoveGuestAgent"
                        )));
                    }
                    _ => {
                        andler_disk::guest_tools::remove_agent_offline(&disk_path, &package)
                            .await?;
                    }
                }
                tracing::info!(
                    instance_id = %id.0,
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
        let (state, disk_path, handle, backend_kind, config_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
                record.config.kind.clone(),
            )
        };

        let mut results = Vec::new();

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                let is_android = matches!(
                    &config_kind,
                    InstanceKind::AndroidVm { .. }
                );
                let packages = if is_android {
                    andler_disk::guest_tools::ANDROID_PACKAGES
                } else {
                    andler_disk::guest_tools::KNOWN_PACKAGES
                };
                for pkg in packages {
                    let installed = backend
                        .guest_check_binary_installed(&handle, pkg.binary_check)
                        .await
                        .unwrap_or(false);
                    results.push((
                        pkg.name.to_string(),
                        pkg.description.to_string(),
                        if installed { "installed" } else { "not_installed" }.to_string(),
                    ));
                }
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                let is_android = matches!(
                    &config_kind,
                    InstanceKind::AndroidVm { .. }
                );
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
                            andler_disk::guest_tools::PackageStatus::NotInstalled => "not_installed",
                            andler_disk::guest_tools::PackageStatus::Unknown => "unknown",
                        }.to_string(),
                    ));
                }
            }
            other => {
                return Err(DaemonError::GuestAgentUnavailable {
                    instance_id: id,
                    message: format!(
                        "instance is in state {other:?}; cannot list packages"
                    ),
                });
            }
        }

        Ok(results)
    }

    pub async fn switch_arm_translator(
        &self,
        id: InstanceId,
        translator: andler_core::android_profile::ArmTranslator,
        translator_dir: Option<PathBuf>,
    ) -> Result<(), DaemonError> {
        let (overlay_path, android_version) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            match &record.config.kind {
                andler_core::config::InstanceKind::AndroidVm { .. } => {}
                _ => return Err(DaemonError::NotAndroid(id)),
            }

            if !record.state.is_disk_idle() {
                return Err(DaemonError::InstanceMustBeStopped(
                    id,
                    record.state.clone(),
                ));
            }

            let android_version = match &record.config.kind {
                andler_core::config::InstanceKind::AndroidVm { android_profile } => {
                    android_profile.android_version.to_string()
                }
                _ => "13".to_string(),
            };

            (
                record.config.disk.path.clone(),
                android_version,
            )
        };

        andler_disk::arm_translator::switch_translator(
            &overlay_path,
            translator,
            translator_dir,
            &android_version,
        )
        .await?;

        tracing::info!(
            instance_id = %id.0,
            translator = ?translator,
            "ARM translator switched successfully"
        );
        Ok(())
    }
    pub async fn set_instance_config(
        &self,
        id: InstanceId,
        key: &str,
        value: &str,
    ) -> Result<(), DaemonError> {
        {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            if !record.state.is_disk_idle() {
                return Err(DaemonError::InstanceMustBeStopped(
                    id,
                    record.state.clone(),
                ));
            }
        }

        match key {
            "name" => {
                let mut instances = self.instances.write().await;
                if let Some(record) = instances.get_mut(&id) {
                    record.config.name = value.to_string();
                }
            }
            "arm_translator" => {
                let translator: andler_core::android_profile::ArmTranslator = value
                    .parse()
                    .map_err(|_| DaemonError::InvalidConfigKey(key.to_string()))?;
                self.switch_arm_translator(id, translator, None).await?;
            }
            _ => return Err(DaemonError::InvalidConfigKey(key.to_string())),
        }

        let (cfg_snapshot, state_snapshot) = {
            let instances = self.instances.read().await;
            let record = instances.get(&id).ok_or(DaemonError::InstanceNotFound(id))?;
            (record.config.clone(), record.state.clone())
        };
        self.persist_config_update(&cfg_snapshot, &state_snapshot)
            .await;

        Ok(())
    }

}

fn spawn_compact_on_shutdown(id: InstanceId, disk: andler_core::DiskConfig) {
    if !disk.compact_on_shutdown {
        return;
    }
    if disk.format != DiskFormat::Qcow2 {
        tracing::debug!(
            instance_id = %id.0,
            format = ?disk.format,
            "compact_on_shutdown is enabled but disk format is not qcow2 — skipping, "
        return;
    }

    let path = disk.path.clone();
    tokio::spawn(async move {
        tracing::info!(
            instance_id = %id.0,
            path = %path.display(),
            "compact_on_shutdown: starting automatic disk compaction"
        );
        match andler_disk::qcow2::compact(&path).await {
            Ok(()) => {
                tracing::info!(
                    instance_id = %id.0,
                    path = %path.display(),
                    "compact_on_shutdown: disk compaction finished"
                );
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %id.0,
                    path = %path.display(),
                    error = %err,
                    "compact_on_shutdown: automatic disk compaction failed"
                );
            }
        }
    });
}
