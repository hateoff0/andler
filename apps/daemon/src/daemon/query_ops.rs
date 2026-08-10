use super::error::DaemonError;
use super::types::InstanceSummary;
use super::Daemon;
use andler_core::{
    BackendStatus, InstanceConfig, InstanceId, InstanceState, LogLine, Resolution, ResourceMetrics,
};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;

impl Daemon {
    pub async fn status(&self, id: InstanceId) -> Result<BackendStatus, DaemonError> {
        let handle = self.handle_for(id).await?;

        let state = handle.state();
        let Some(backend_handle) = handle.backend_handle() else {
            return Ok(BackendStatus {
                state,
                detail: None,
                clean_shutdown: false,
            });
        };

        // The FSM state is the single source of truth; the backend is only
        // consulted while the instance is actually expected to be alive.
        // A backend that reports the process gone (e.g. right after a crash,
        // before the exit watcher's Fail lands) must not override the FSM.
        if !matches!(state, InstanceState::Running | InstanceState::Paused) {
            return Ok(BackendStatus {
                state,
                detail: None,
                clean_shutdown: false,
            });
        }

        let backend = self.backend_for(handle.config().backend)?;
        let status = backend
            .status(&backend_handle)
            .await
            .map_err(DaemonError::Backend)?;

        if status.state == state {
            Ok(status)
        } else {
            Ok(BackendStatus {
                state,
                detail: Some(format!(
                    "backend reports {:?}, supervisor transition pending",
                    status.state
                )),
                clean_shutdown: false,
            })
        }
    }

    pub async fn stream_instance_logs(
        &self,
        id: InstanceId,
    ) -> Result<BoxStream<'static, LogLine>, DaemonError> {
        let handle = self.handle_for(id).await?;

        let backend_handle = match handle.backend_handle() {
            Some(backend_handle) => backend_handle,
            None => return Ok(Box::pin(futures_util::stream::empty())),
        };
        let backend = self.backend_for(handle.config().backend)?.clone();

        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.log_stream(&backend_handle);
            while let Some(line) = inner.next().await {
                yield line;
            }
        }))
    }

    pub async fn stream_resource_metrics(
        &self,
        id: InstanceId,
    ) -> Result<BoxStream<'static, ResourceMetrics>, DaemonError> {
        let handle = self.handle_for(id).await?;

        let backend_handle = match handle.backend_handle() {
            Some(backend_handle) => backend_handle,
            None => return Ok(Box::pin(futures_util::stream::empty())),
        };
        let backend = self.backend_for(handle.config().backend)?.clone();

        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.metrics_stream(&backend_handle);
            while let Some(metrics) = inner.next().await {
                yield metrics;
            }
        }))
    }

    pub async fn list_instances(&self) -> Vec<InstanceSummary> {
        let supervisors = self.supervisors.read().await;
        let mut summaries: Vec<InstanceSummary> = supervisors
            .values()
            .map(|handle| {
                let config = handle.config();
                InstanceSummary {
                    id: config.id,
                    name: config.name.clone(),
                    state: handle.state(),
                    broken_reason: None,
                }
            })
            .collect();

        let broken = self.broken.read().await;
        summaries.extend(broken.iter().map(|(id, reason)| InstanceSummary {
            id: *id,
            name: String::new(),
            state: InstanceState::Error {
                message: reason.clone(),
            },
            broken_reason: Some(reason.clone()),
        }));
        summaries.sort_by_key(|summary| summary.id.to_string());
        summaries
    }

    /// File-vs-memory diff for `config status`, after a reload so manual
    /// edits made since the last operation are visible.
    pub async fn config_status(
        &self,
        id: InstanceId,
    ) -> Result<
        (
            InstanceConfig,
            Vec<(String, String, String)>,
            Option<String>,
            Option<Resolution>,
        ),
        DaemonError,
    > {
        let handle = self.handle_for(id).await?;
        let snapshot = handle.reload_config().await?;
        let memory = &snapshot.config;
        let file = snapshot.file_config.as_ref().unwrap_or(memory);
        let diffs = andler_core::config::diff_configs(file, memory);
        Ok((
            snapshot.config,
            diffs,
            snapshot.file_error,
            snapshot.live_resolution,
        ))
    }

    pub async fn get_instance_config(&self, id: InstanceId) -> Result<InstanceConfig, DaemonError> {
        let handle = self.handle_for(id).await?;
        Ok(handle.config())
    }

    pub async fn update_instance_config(
        &self,
        id: InstanceId,
        new_config: InstanceConfig,
    ) -> Result<(), DaemonError> {
        if new_config.id != id {
            return Err(DaemonError::ConfigIdMismatch {
                expected: id,
                actual: new_config.id,
            });
        }
        new_config.validate().map_err(DaemonError::InvalidConfig)?;

        let handle = self.handle_for(id).await?;

        let current = handle.config();
        let state = handle.state();

        if !state.is_disk_idle() {
            return Err(DaemonError::InstanceMustBeStopped(id, state));
        }

        let kind_changed =
            std::mem::discriminant(&current.kind) != std::mem::discriminant(&new_config.kind);
        if kind_changed {
            return Err(DaemonError::ConfigKindChanged(id));
        }
        if current.disk.path != new_config.disk.path {
            return Err(DaemonError::ConfigDiskPathChanged(id));
        }

        // Deprecated path (CLI `config edit` now edits the file directly):
        // keep file and memory in sync so callers of the RPC still observe
        // the TOML as the source of truth.
        let instance_dir = new_config.disk.path.parent().map(std::path::PathBuf::from);
        if let Some(dir) = instance_dir {
            super::types::write_instance_toml(&dir, &new_config).await;
        }
        handle.set_config(new_config, None).await?;

        Ok(())
    }
}
