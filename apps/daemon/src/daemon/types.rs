use std::path::PathBuf;

use andler_core::{InstanceConfig, InstanceId, InstanceState};

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

    // tmp+rename so a concurrent reader never observes a half-written file.
    let path = instance_dir.join("instance.toml");
    let tmp_path = instance_dir.join("instance.toml.tmp");
    if let Err(err) = tokio::fs::write(&tmp_path, toml_string).await {
        tracing::error!(
            instance_id = %cfg.id,
            path = %tmp_path.display(),
            error = %err,
            "failed to write instance.toml (instance was still created successfully)"
        );
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp_path, &path).await {
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

    let instance_dir = config.disk.path.parent();
    for disk in &config.extra_disks {
        if disk.path.parent() != instance_dir {
            continue;
        }
        if let Err(err) = tokio::fs::remove_file(&disk.path).await {
            tracing::error!(
                instance_id = %id,
                path = %disk.path.display(),
                error = %err,
                "purge: failed to remove extra disk file"
            );
        }
    }

    for audit_file in ["instance.toml", "events.jsonl"] {
        if let Some(dir) = instance_dir {
            let path = dir.join(audit_file);
            if let Err(err) = tokio::fs::remove_file(&path).await {
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::error!(
                        instance_id = %id,
                        path = %path.display(),
                        error = %err,
                        "purge: failed to remove {audit_file}"
                    );
                }
            }
        }
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

/// Removes the registry entries (instance.toml, events.jsonl) for a removed
/// instance without touching its disk or firmware files.
pub async fn remove_registry_entries(config: &InstanceConfig) {
    let Some(dir) = config.disk.path.parent() else {
        return;
    };
    for file in ["instance.toml", "events.jsonl"] {
        let path = dir.join(file);
        if let Err(err) = tokio::fs::remove_file(&path).await {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::error!(
                    instance_id = %config.id,
                    path = %path.display(),
                    error = %err,
                    "remove: failed to remove {file}"
                );
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
    /// Set for registry entries whose instance.toml is missing or unreadable;
    /// such entries carry no config, so name/state are placeholders.
    pub broken_reason: Option<String>,
}
