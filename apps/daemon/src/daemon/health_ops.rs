use super::error::DaemonError;
use super::Daemon;
use andler_core::{InstanceEvent, InstanceId, InstanceState};

impl Daemon {
    pub async fn run_health_check_once(&self) {
        let running: Vec<_> = {
            let supervisors = self.supervisors.read().await;
            supervisors
                .iter()
                .filter_map(|(id, handle)| {
                    if handle.state() == InstanceState::Running {
                        let config = handle.config();
                        handle
                            .backend_handle()
                            .map(|backend_handle| (*id, config.backend, backend_handle))
                    } else {
                        None
                    }
                })
                .collect()
        };

        for (id, backend_kind, backend_handle) in running {
            let backend = match self.backend_for(backend_kind) {
                Ok(backend) => backend.clone(),
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id,
                        error = %err,
                        "health check: no backend registered, skipping"
                    );
                    continue;
                }
            };

            let status = match backend.status(&backend_handle).await {
                Ok(status) => status,
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id,
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

            let reason = status.detail.clone().unwrap_or_else(|| {
                format!("backend now reports state {:?}, was Running", status.state)
            });

            if status.clean_shutdown {
                tracing::info!(
                    instance_id = %id,
                    reason = %reason,
                    "instance health check: guest shut down cleanly — marking Stopped"
                );
                if let Err(err) = self.mark_instance_stopped_cleanly(id).await {
                    tracing::error!(
                        instance_id = %id,
                        error = %err,
                        "health check: failed to record clean shutdown in FSM"
                    );
                }
                continue;
            }

            tracing::error!(
                instance_id = %id,
                reason = %reason,
                "instance health check: process is no longer running (was Running) \
                 — marking Error. Restart it manually with `andler start`."
            );

            if let Err(err) = self.mark_instance_crashed(id, reason).await {
                tracing::error!(
                    instance_id = %id,
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
        let handle = self.handle_for(id).await?;
        handle.set_handle(None).await?;
        handle.transition(InstanceEvent::Fail(reason)).await?;
        Ok(())
    }

    /// Like `mark_instance_crashed`, but for a backend-observed status that reflects
    /// a genuine guest-initiated shutdown (see `BackendStatus::clean_shutdown`) rather
    /// than the process disappearing unexpectedly — reaches `Stopped` through the
    /// normal Stop+StopCompleted transitions instead of `Error`.
    pub(crate) async fn mark_instance_stopped_cleanly(
        &self,
        id: InstanceId,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        handle.set_handle(None).await?;
        handle.transition(InstanceEvent::Stop).await?;
        handle.transition(InstanceEvent::StopCompleted).await?;
        Ok(())
    }
}
