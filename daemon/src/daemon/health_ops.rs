

use super::error::DaemonError;
use super::Daemon;
use andler_core::{InstanceEvent, InstanceId, InstanceState};

impl Daemon {

    pub async fn run_health_check_once(&self) {
        let running: Vec<_> = {
            let instances = self.instances.read().await;
            instances
                .iter()
                .filter_map(|(id, record)| {
                    if record.state == InstanceState::Running {
                        record
                            .handle
                            .clone()
                            .map(|handle| (*id, record.config.backend, handle))
                    } else {
                        None
                    }
                })
                .collect()
        };

        for (id, backend_kind, handle) in running {
            let backend = match self.backend_for(backend_kind) {
                Ok(backend) => backend.clone(),
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id.0,
                        error = %err,
                        "health check: no backend registered, skipping"
                    );
                    continue;
                }
            };

            let status = match backend.status(&handle).await {
                Ok(status) => status,
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id.0,
                        error = %err,
                        "health check: status query failed, will retry next cycle"
                    );
                    continue;
                }
            };

            let still_active = matches!(
                status.state,
                InstanceState::Running | InstanceState::Paused | InstanceState::Starting
            );
            if still_active {
                continue;
            }

            let reason = status.detail.unwrap_or_else(|| {
                format!(
                    "backend now reports state {:?}, was Running",
                    status.state
                )
            });
            tracing::error!(
                instance_id = %id.0,
                reason = %reason,
                "instance health check: process is no longer running (was Running) \
                 — marking Error. Restart it manually with `andler start`."
            );

            if let Err(err) = self.mark_instance_crashed(id, reason).await {
                tracing::error!(
                    instance_id = %id.0,
                    error = %err,
                    "health check: failed to record crash in FSM"
                );
            }
        }
    }

    pub(crate) async fn mark_instance_crashed(
        &self,
        id: InstanceId,
        reason: String,
    ) -> Result<(), DaemonError> {
        let final_state = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get_mut(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            record.handle = None;
            record.state = record.state.clone().apply(InstanceEvent::Fail(reason))?;
            record.state.clone()
        };
        self.persist_state(id, &final_state).await;
        Ok(())
    }
}
