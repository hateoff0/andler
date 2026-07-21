use super::Daemon;
use super::error::DaemonError;
use super::types::InstanceSummary;
use andler_core::{BackendStatus, InstanceConfig, InstanceId, LogLine, ResourceMetrics};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;

impl Daemon {

    pub async fn status(&self, id: InstanceId) -> Result<BackendStatus, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        match &record.handle {
            Some(handle) => {
                let backend = self.backend_for(record.config.backend)?;
                backend.status(handle).await.map_err(DaemonError::Backend)
            }
            None => Ok(BackendStatus {
                state: record.state.clone(),
                detail: Some("instance has no running backend handle".to_string()),
            }),
        }
    }


    pub async fn stream_instance_logs(
        &self,
        id: InstanceId,
    ) -> Result<BoxStream<'static, LogLine>, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = match record.handle.clone() {
            Some(handle) => handle,
            None => return Ok(Box::pin(futures_util::stream::empty())),
        };
        let backend = self.backend_for(record.config.backend)?.clone();

        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.log_stream(&handle);
            while let Some(line) = inner.next().await {
                yield line;
            }
        }))
    }


    pub async fn stream_resource_metrics(
        &self,
        id: InstanceId,
    ) -> Result<BoxStream<'static, ResourceMetrics>, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = match record.handle.clone() {
            Some(handle) => handle,
            None => return Ok(Box::pin(futures_util::stream::empty())),
        };
        let backend = self.backend_for(record.config.backend)?.clone();

        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.metrics_stream(&handle);
            while let Some(metrics) = inner.next().await {
                yield metrics;
            }
        }))
    }


    pub async fn list_instances(&self) -> Vec<InstanceSummary> {
        let instances = self.instances.read().await;
        instances
            .values()
            .map(|record| InstanceSummary {
                id: record.config.id,
                name: record.config.name.clone(),
                state: record.state.clone(),
            })
            .collect()
    }


    pub async fn get_instance_config(&self, id: InstanceId) -> Result<InstanceConfig, DaemonError> {
        let instances = self.instances.read().await;
        instances
            .get(&id)
            .map(|record| record.config.clone())
            .ok_or(DaemonError::InstanceNotFound(id))
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

        let (cfg_snapshot, state_snapshot) = {
            let mut instances = self.instances.write().await;
            let record = instances.get_mut(&id).ok_or(DaemonError::InstanceNotFound(id))?;

            let kind_changed = std::mem::discriminant(&record.config.kind)
                != std::mem::discriminant(&new_config.kind);
            if kind_changed {
                return Err(DaemonError::ConfigKindChanged(id));
            }
            if record.config.disk.path != new_config.disk.path {
                return Err(DaemonError::ConfigDiskPathChanged(id));
            }

            record.config = new_config;
            (record.config.clone(), record.state.clone())
        };

        self.persist_config_update(&cfg_snapshot, &state_snapshot).await;

        Ok(())
    }
}
