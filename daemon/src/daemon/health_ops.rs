//! Периодическая проверка живости запущенных инстансов — см. ROADMAP.md,
//! "Core: add VM health checks and auto-restart on failure".
//!
//! Обнаруживает случай, когда процесс гипервизора инстанса, который
//! daemon считает `Running`, на самом деле уже завершился не через
//! штатный путь `stop_instance` (упал, был убит снаружи процесса и
//! т.п.) — ни один штатный вызов backend'а в этом случае не происходит,
//! поэтому FSM-запись daemon'а протухает и никогда не узнает об этом
//! сама по себе без отдельного polling'а. Вызывается периодически из
//! `daemon/src/main.rs` (интервал — `ANDLERD_HEALTH_CHECK_INTERVAL_SECS`).
//!
//! ## Про "auto-restart" из названия пункта ROADMAP.md
//!
//! Этот модуль **сознательно не перезапускает** упавшие инстансы
//! автоматически сам — только обнаруживает и переводит FSM в `Error`.
//! Изначально это было архитектурным ограничением (`Stopped`/`Error`
//! не принимали `Start` вовсе), но это уже не так —
//! `andler_core::fsm` теперь разрешает `Start` из обоих (см. doc-
//! комментарий там), `Daemon::start_instance` можно вызвать вручную
//! (`andler start <id>`) сразу после того, как этот модуль пометил
//! инстанс `Error`. Технической причины не сделать это автоматически
//! отсюда больше нет — оставлено ручным решением осознанно, а не из-за
//! ограничения: silent auto-restart упавшего инстанса без спрашивания
//! пользователя рискует замаскировать реально ломающуюся конфигурацию
//! (например, повреждённый диск) бесконечным циклом падений/перезапусков
//! без явного лимита попыток/backoff — эта политика (сколько раз, с
//! какой паузой, когда сдаться) требует отдельного решения, не то, что
//! стоит незаметно приложить к чистой detection-фиче.

use super::error::DaemonError;
use super::Daemon;
use andler_core::{InstanceEvent, InstanceId, InstanceState};

impl Daemon {
    /// Один проход проверки: для каждой записи в состоянии `Running`
    /// спрашивает у backend'а реальный статус процесса; если тот
    /// оказался не `Running`/`Paused`/`Starting` (то есть backend считает
    /// процесс уже не активным), переводит FSM-запись в `Error` и
    /// персистит — см. doc-комментарий модуля про то, почему не
    /// перезапускает автоматически.
    ///
    /// Не трогает `Paused` (пауза намеренная, не крах) и `Stopping`
    /// (уже в процессе штатного перехода, за которым следит сам
    /// `stop_instance`) — только `Running`, единственное состояние, в
    /// котором "backend внезапно говорит иначе" однозначно означает
    /// незамеченный крах, а не что-то ожидаемое.
    ///
    /// Ошибки самого опроса статуса (`backend.status()` вернул `Err`,
    /// например временный сбой QMP) не считаются крахом сами по себе —
    /// логируются и пропускаются до следующего цикла, а не интерпретируются
    /// как "процесс умер": транзиентная ошибка опроса — не то же самое,
    /// что подтверждённое `BackendStatus { state: Stopped/Error, .. }`.
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
            // The handle refers to a process the backend itself no longer
            // considers running — don't leave a dangling handle around
            // for a later operation to (mis)use.
            record.handle = None;
            record.state = record.state.clone().apply(InstanceEvent::Fail(reason))?;
            record.state.clone()
        };
        self.persist_state(id, &final_state).await;
        Ok(())
    }
}
