use std::path::PathBuf;

use super::error::DaemonError;
use super::Daemon;
use andler_core::{CloneMode, InstanceConfig, InstanceId, InstanceKind, InstanceState};

impl Daemon {
    pub async fn clone_instance(
        &self,
        source_id: InstanceId,
        new_name: String,
        instances_root: PathBuf,
        mode: CloneMode,
    ) -> Result<InstanceId, DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;

        if mode == CloneMode::SharedBase
            && !matches!(source_config.kind, InstanceKind::AndroidVm { .. })
        {
            return Err(DaemonError::SharedBaseNotSupportedForLinuxVm(source_id));
        }

        let new_id = InstanceId::new();
        let instance_dir = instances_root.join(new_id.to_string());
        let mut dir_guard = super::types::InstanceDirGuard::new(instance_dir.clone());

        andler_core::paths::ensure_private_dir(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        let new_ovmf_vars_path = instance_dir.join("VARS.fd");
        tokio::fs::copy(&source_config.firmware.ovmf_vars_path, &new_ovmf_vars_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: new_ovmf_vars_path.clone(),
                source,
            })?;

        let new_disk_path = instance_dir.join("disk.qcow2");
        let cloned_disk = match mode {
            CloneMode::Linked => {
                andler_disk::clone::linked_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    source_config.disk.size_bytes,
                )
                .await?
            }
            CloneMode::FullStandalone => {
                andler_disk::clone::full_standalone_clone(&source_config.disk.path, &new_disk_path)
                    .await?
            }
            CloneMode::SharedBase => {
                let base_image =
                    source_config
                        .disk
                        .base_image
                        .clone()
                        .ok_or_else(|| DaemonError::Io {
                            path: source_config.disk.path.clone(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "SharedBase clone requires a source disk with base_image set",
                            ),
                        })?;
                andler_disk::clone::shared_base_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    &base_image,
                )
                .await?
            }
        };

        let mut new_config = source_config;
        new_config.id = new_id;
        new_config.name = new_name;
        new_config.disk.path = cloned_disk.disk_path;
        new_config.disk.base_image = cloned_disk.backing_file;
        new_config.firmware.ovmf_vars_path = new_ovmf_vars_path;

        super::types::write_instance_toml(&instance_dir, &new_config).await;

        let registered_id = self.create_instance_in(new_config, instance_dir).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    pub async fn export_instance_disk(
        &self,
        source_id: InstanceId,
        dest_path: PathBuf,
    ) -> Result<(), DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;

        andler_disk::clone::full_standalone_clone(&source_config.disk.path, &dest_path).await?;

        Ok(())
    }

    async fn terminal_clonable_instance_config(
        &self,
        id: InstanceId,
    ) -> Result<InstanceConfig, DaemonError> {
        let handle = self.handle_for(id).await?;
        let (state, config) = (handle.state(), handle.config());

        let clonable = matches!(
            state,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        );
        if !clonable {
            return Err(DaemonError::InstanceNotClonable(id, state));
        }

        Ok(config)
    }

    pub async fn find_live_clones(&self, id: InstanceId) -> Result<Vec<InstanceId>, DaemonError> {
        let supervisors = self.supervisors.read().await;
        let target_disk_path = supervisors
            .get(&id)
            .map(|handle| handle.config().disk.path.clone())
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let clones = supervisors
            .iter()
            .filter(|(other_id, handle)| {
                **other_id != id
                    && handle.config().disk.base_image.as_deref()
                        == Some(target_disk_path.as_path())
            })
            .map(|(other_id, _)| *other_id)
            .collect();

        Ok(clones)
    }
}
