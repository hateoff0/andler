use super::error::DaemonError;
use super::types::InstanceSummary;
use super::Daemon;
use andler_core::{BackendStatus, InstanceConfig, InstanceId, LogLine, ResourceMetrics};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;

impl Daemon {
    pub async fn status(&self, id: InstanceId) -> Result<BackendStatus, DaemonError> {
        let handle = self.handle_for(id).await?;

        let Some(backend_handle) = handle.backend_handle() else {
            return Ok(BackendStatus {
                state: handle.state(),
                detail: Some("instance has no running backend handle".to_string()),
                clean_shutdown: false,
            });
        };
        let backend = self.backend_for(handle.config().backend)?;
        backend
            .status(&backend_handle)
            .await
            .map_err(DaemonError::Backend)
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
        supervisors
            .values()
            .map(|handle| {
                let config = handle.config();
                InstanceSummary {
                    id: config.id,
                    name: config.name.clone(),
                    state: handle.state(),
                }
            })
            .collect()
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

        handle.set_config(new_config).await?;

        Ok(())
    }
}
