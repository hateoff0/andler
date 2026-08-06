use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::error::DaemonError;
use super::Daemon;
use andler_core::{
    BackendError, DiskConfig, DiskFormat, HypervisorBackend, InstanceId, InstanceState,
};

const EXTRA_DISK_FILENAME: &str = "disk-extra";

impl Daemon {
    /// Returns (backend, handle) for hotplug operations, which require the
    /// instance to be Running or Paused. Distinct from `with_running_instance`
    /// (whose error message is snapshot-specific), so failed state gates point
    /// the user at `andler start` rather than at snapshots.
    async fn hotplug_target(
        &self,
        id: InstanceId,
    ) -> Result<(Arc<dyn HypervisorBackend>, andler_core::BackendHandle), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        if !matches!(record.state, InstanceState::Running | InstanceState::Paused) {
            return Err(DaemonError::HotplugRequiresRunningInstance(
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

    pub async fn attach_disk(
        &self,
        id: InstanceId,
        requested_path: Option<PathBuf>,
        size_bytes: u64,
    ) -> Result<(PathBuf, usize), DaemonError> {
        let (backend, handle) = self.hotplug_target(id).await?;

        let (instance_dir, index) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            let instance_dir = record
                .config
                .disk
                .path
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| {
                    DaemonError::InvalidConfig(format!(
                        "instance {id:?} disk has no parent directory"
                    ))
                })?;
            (instance_dir, record.config.extra_disks.len())
        };
        let disk_path = resolve_extra_disk_path(requested_path, &instance_dir, index)?;

        {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            if record.config.disk.path == disk_path
                || record
                    .config
                    .extra_disks
                    .iter()
                    .any(|d| d.path == disk_path)
            {
                return Err(DaemonError::DiskAlreadyAttached(id, disk_path));
            }
        }

        let created = !disk_path.exists();
        let actual_size = if created {
            if size_bytes == 0 {
                return Err(DaemonError::InvalidConfig(
                    "disk size must be greater than 0 when the image does not exist yet"
                        .to_string(),
                ));
            }
            andler_disk::qcow2::create(&disk_path, size_bytes).await?;
            size_bytes
        } else {
            andler_disk::qcow2::virtual_size_bytes(&disk_path).await?
        };
        if actual_size == 0 {
            if created {
                let _ = tokio::fs::remove_file(&disk_path).await;
            }
            return Err(DaemonError::InvalidConfig(format!(
                "disk {} has size 0; attach a real disk image or pass --size",
                disk_path.display()
            )));
        }

        let cfg = DiskConfig {
            path: disk_path.clone(),
            size_bytes: actual_size,
            format: DiskFormat::Qcow2,
            base_image: None,
            thin_provisioning: true,
            trim_on_shutdown: false,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        };

        if let Err(err) = backend.attach_disk(&handle, &cfg, index).await {
            if created {
                let _ = tokio::fs::remove_file(&disk_path).await;
            }
            return Err(err.into());
        }

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;
        record.config.extra_disks.push(cfg);
        let (cfg, state) = (record.config.clone(), record.state.clone());
        drop(instances);
        self.persist_config_update(&cfg, &state).await;

        Ok((disk_path, index))
    }

    pub async fn detach_disk(&self, id: InstanceId, path: PathBuf) -> Result<(), DaemonError> {
        let (backend, handle, index) = {
            let (backend, handle) = self.hotplug_target(id).await?;
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            let index = record
                .config
                .extra_disks
                .iter()
                .position(|d| d.path == path)
                .ok_or_else(|| DaemonError::DiskNotAttached(id, path.clone()))?;
            (backend, handle, index)
        };

        backend.detach_disk(&handle, index).await?;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;
        record.config.extra_disks.remove(index);
        let (cfg, state) = (record.config.clone(), record.state.clone());
        drop(instances);
        self.persist_config_update(&cfg, &state).await;
        Ok(())
    }

    pub async fn attach_network(
        &self,
        id: InstanceId,
        network: andler_core::NetworkConfig,
    ) -> Result<usize, DaemonError> {
        if network.device_model.is_empty() {
            return Err(DaemonError::InvalidConfig(
                "network device model must not be empty".to_string(),
            ));
        }

        let (backend, handle) = self.hotplug_target(id).await?;
        let index = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            record.config.extra_networks.len()
        };
        backend.attach_network(&handle, &network, index).await?;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;
        record.config.extra_networks.push(network);
        let (cfg, state) = (record.config.clone(), record.state.clone());
        drop(instances);
        self.persist_config_update(&cfg, &state).await;

        Ok(index)
    }

    pub async fn detach_network(&self, id: InstanceId, index: usize) -> Result<(), DaemonError> {
        let (backend, handle) = self.hotplug_target(id).await?;
        {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            let attached = record.config.extra_networks.len();
            if index >= attached {
                return Err(DaemonError::NetworkNotAttached {
                    instance_id: id,
                    index,
                    attached,
                });
            }
        }
        backend.detach_network(&handle, index).await?;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;
        record.config.extra_networks.remove(index);
        let (cfg, state) = (record.config.clone(), record.state.clone());
        drop(instances);
        self.persist_config_update(&cfg, &state).await;
        Ok(())
    }
}

fn resolve_extra_disk_path(
    requested: Option<PathBuf>,
    instance_dir: &Path,
    index: usize,
) -> Result<PathBuf, DaemonError> {
    let path = match requested {
        // Explicit plain file name -> place it inside the instance directory so
        // it follows the instance, mirroring the primary disk layout.
        Some(path) if path.is_relative() && path.parent().is_none() => instance_dir.join(path),
        Some(path) => path,
        None => instance_dir.join(format!("{EXTRA_DISK_FILENAME}{index}.qcow2")),
    };
    if !path.is_absolute() {
        return Err(DaemonError::InvalidConfig(format!(
            "disk path {} must be absolute (or a plain file name to place in the instance directory)",
            path.display()
        )));
    }
    Ok(path)
}
