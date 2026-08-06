use std::path::PathBuf;

use andler_core::{BackendHandle, InstanceConfig, InstanceId, InstanceState};

pub(crate) struct InstanceRecord {
    pub(crate) config: InstanceConfig,
    pub(crate) state: InstanceState,
    pub(crate) handle: Option<BackendHandle>,
}

#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub id: uuid::Uuid,
    pub instance_id: InstanceId,
    pub tag: String,
    pub description: Option<String>,
    pub created_at: String,
}

pub(crate) struct InstanceDirGuard {
    pub(crate) path: PathBuf,
    armed: bool,
}

impl InstanceDirGuard {
    pub fn new(path: PathBuf) -> Self {
        InstanceDirGuard { path, armed: true }
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InstanceDirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(crate) async fn write_instance_toml(instance_dir: &std::path::Path, cfg: &InstanceConfig) {
    let toml_string = match toml::to_string_pretty(cfg) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(
                instance_id = %cfg.id,
                error = %err,
                "failed to serialize instance.toml (instance was still created successfully)"
            );
            return;
        }
    };

    let path = instance_dir.join("instance.toml");
    if let Err(err) = tokio::fs::write(&path, toml_string).await {
        tracing::error!(
            instance_id = %cfg.id,
            path = %path.display(),
            error = %err,
            "failed to write instance.toml (instance was still created successfully)"
        );
    }
}

pub(crate) async fn purge_instance_files(id: InstanceId, config: &InstanceConfig) {
    if let Err(err) = tokio::fs::remove_file(&config.disk.path).await {
        tracing::error!(
            instance_id = %id,
            path = %config.disk.path.display(),
            error = %err,
            "purge: failed to remove instance disk file"
        );
    }

    if let Err(err) = tokio::fs::remove_file(&config.firmware.ovmf_vars_path).await {
        tracing::error!(
            instance_id = %id,
            path = %config.firmware.ovmf_vars_path.display(),
            error = %err,
            "purge: failed to remove instance OVMF_VARS file"
        );
    }

    if let Some(parent) = config.disk.path.parent() {
        let is_instance_dir = parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == id.to_string());

        if is_instance_dir {
            if let Err(err) = tokio::fs::remove_dir_all(parent).await {
                tracing::error!(
                    instance_id = %id,
                    path = %parent.display(),
                    error = %err,
                    "purge: failed to remove instance directory"
                );
            }
        } else {
            let _ = tokio::fs::remove_dir(parent).await;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
}
