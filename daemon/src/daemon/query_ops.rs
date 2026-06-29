use super::Daemon;
use super::error::DaemonError;
use super::types::InstanceSummary;
use andler_core::{BackendStatus, InstanceConfig, InstanceId, LogLine, ResourceMetrics};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;

impl Daemon {
    /// Текущий статус инстанса. Для инстансов без запущенного backend'а
    /// (ещё не стартовали, либо уже остановлены и `handle` сброшен)
    /// возвращает состояние FSM записи демона напрямую, без обращения к
    /// backend'у — у backend'а просто нет хэндла, который можно было бы
    /// спросить.
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

    /// Поток строк stdout/stderr процесса гипервизора инстанса (см.
    /// `andler_core::LogLine`) — live-tail с момента вызова, без истории
    /// (см. документацию `HypervisorBackend::log_stream` за обоснованием).
    ///
    /// Для инстанса без запущенного backend'а (`record.handle == None`,
    /// тот же случай, что у `status()` выше) возвращает немедленно
    /// завершающийся пустой поток, не ошибку — наблюдать за процессом,
    /// которого сейчас нет, означает "сейчас нечего показать", не
    /// "невалидный запрос" (см. документацию `HypervisorBackend::log_stream`
    /// за тем, почему это сознательно отличается от `pause_instance`/
    /// `resume_instance` с тем же отсутствующим хэндлом).
    ///
    /// Возвращаемый поток — `'static` и не заимствует `&self`: внутри
    /// держит собственный клон `Arc<dyn HypervisorBackend>` (backend'ы
    /// уже хранятся как `Arc` в `self.backends`, клонирование — это просто
    /// инкремент счётчика ссылок, не глубокое копирование) и сам
    /// `BackendHandle`, поэтому переживает возврат из этого метода — это
    /// необходимо: gRPC-хендлер (`andler-rpc`/`service.rs`) будет
    /// поллить этот поток уже после того, как вызов `stream_instance_logs`
    /// завершился и любые ссылки на `Daemon` из этого вызова вышли из
    /// скоупа.
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

        // `async_stream::stream!` строит `Stream`, которому позволено
        // владеть `backend`/`handle` внутри собственного тела генератора
        // — отсюда и `'static` результат, в отличие от прямого
        // `backend.log_stream(&handle)`, чей `BoxStream<'_, LogLine>`
        // заимствовал бы `backend` на время жизни этого вызова. Сам
        // внутренний поток подписывается на `broadcast`-канал процесса
        // только один раз, при первом полле (а не при каждом вызове
        // `log_stream`), и дальше просто транслирует его элементы —
        // подписка происходит ровно там же, где произошла бы при прямом
        // вызове `backend.log_stream(&handle)`.
        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.log_stream(&handle);
            while let Some(line) = inner.next().await {
                yield line;
            }
        }))
    }

    /// Стрим метрик ресурсов для инстанса — аналог `stream_instance_logs`
    /// по паттерну: сервер-стриминг через `broadcast`-канал процесса.
    ///
    /// Если инстанс не найден, не имеет запущенного backend, или backend
    /// не реализует `metrics_stream` — возвращает пустой поток (не ошибку),
    /// как и `stream_instance_logs` (см. документацию
    /// `HypervisorBackend::metrics_stream` за контрактом).
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

    /// Сводка по всем зарегистрированным инстансам — единственный способ
    /// узнать, какие `InstanceId` вообще существуют, без необходимости
    /// заранее знать их (например, если вывод `andler create` потерян:
    /// закрыт терминал, не сохранён stdout скрипта — без `list_instances`
    /// созданный инстанс физически существует в `Daemon`/`Store`, но
    /// недостижим ни для одной другой команды, которым всем нужен
    /// `InstanceId` на входе).
    ///
    /// Сознательно возвращает только состояние записи демона
    /// (`record.state`), а не реальный backend-статус через
    /// `backend.status(handle)`, как делает `status()` для запущенных
    /// инстансов — обзорный список не должен порождать по одному
    /// сетевому/процессному запросу на каждый инстанс (для QEMU это QMP
    /// round-trip), когда вызывающему обычно нужен только список
    /// id/имён/грубых состояний, а не точный live-статус каждого. Для
    /// точного статуса конкретного инстанса остаётся `status(id)`.
    ///
    /// Порядок записей не гарантирован — источник (`HashMap`) сам по себе
    /// не упорядочен; вызывающая сторона (`andler-cli`) сортирует, если
    /// ей это нужно для вывода.
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

    /// Возвращает полную конфигурацию инстанса по его `InstanceId`.
    ///
    /// Возвращает клон `InstanceConfig` (он уже `Clone` — см.
    /// `andler-core::config::instance`), не ссылку — вызывающая сторона
    /// (`service.rs`) должна владеть значением, чтобы сконвертировать его
    /// в proto-ответ уже после того, как read-lock `instances` отпущен.
    pub async fn get_instance_config(&self, id: InstanceId) -> Result<InstanceConfig, DaemonError> {
        let instances = self.instances.read().await;
        instances
            .get(&id)
            .map(|record| record.config.clone())
            .ok_or(DaemonError::InstanceNotFound(id))
    }
}
