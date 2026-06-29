use super::Daemon;
use super::error::DaemonError;
use super::types::{SnapshotRecord};
use andler_core::{BackendError, InstanceId, InstanceState};

impl Daemon {
    /// Создаёт снапшот инстанса с указанным тегом.
    ///
    /// Тег — произвольная строка, уникальная в пределах инстанса.
    ///
    /// Алгоритм:
    /// 1. Проверить FSM: `Running`/`Paused`
    /// 2. Вызвать `backend.snapshot(handle, tag)` (async job через QMP)
    /// 3. Сохранить метаданные в `store`
    pub async fn create_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        description: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<SnapshotRecord, DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let snapshottable = matches!(
                record.state,
                InstanceState::Running | InstanceState::Paused
            );
            if !snapshottable {
                return Err(DaemonError::SnapshotOperationRequiresRunningInstance(
                    id,
                    record.state.clone(),
                ));
            }

            let handle = record
                .handle
                .clone()
                .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;

            (
                self.backend_for(record.config.backend)?.clone(),
                handle,
            )
        };

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend.snapshot(&handle, &tag, timeout).await?;

        let record = SnapshotRecord {
            id: uuid::Uuid::new_v4(),
            instance_id: id,
            tag: tag.clone(),
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        if let Some(store) = &self.store {
            let stored = andler_store::StoredSnapshot {
                id: record.id,
                instance_id: record.instance_id,
                tag: record.tag.clone(),
                description: record.description.clone(),
                created_at: record.created_at.clone(),
            };
            if let Err(e) = store.save_snapshot(&stored).await {
                tracing::warn!(instance_id = %id, tag = %tag, error = %e, "failed to persist snapshot metadata");
            }
        }

        Ok(record)
    }

    /// Восстанавливает инстанс из снапшота.
    ///
    /// Инстанс должен быть запущен (`Running`/`Paused`) — QEMU snapshot-load
    /// выполняется через QMP, который требует живой процесс. После
    /// восстановления инстанс остаётся в текущем состоянии (Running/Paused);
    /// клиент может явно вызвать `stop_instance` для остановки.
    pub async fn restore_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        timeout_secs: Option<u64>,
    ) -> Result<(), DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let running = matches!(
                record.state,
                InstanceState::Running | InstanceState::Paused
            );
            if !running {
                return Err(DaemonError::SnapshotOperationRequiresRunningInstance(
                    id,
                    record.state.clone(),
                ));
            }

            let handle = record
                .handle
                .clone()
                .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;

            (
                self.backend_for(record.config.backend)?.clone(),
                handle,
            )
        };

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend.snapshot_restore(&handle, &tag, timeout).await?;
        Ok(())
    }

    /// Удаляет снапшот инстанса.
    ///
    /// Инстанс должен быть запущен (`Running`/`Paused`) — QEMU snapshot-delete
    /// выполняется через QMP, который требует живой процесс. Нельзя удалить
    /// снапшот, на который ссылается текущее состояние диска.
    pub async fn delete_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        timeout_secs: Option<u64>,
    ) -> Result<(), DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let running = matches!(
                record.state,
                InstanceState::Running | InstanceState::Paused
            );
            if !running {
                return Err(DaemonError::SnapshotOperationRequiresRunningInstance(
                    id,
                    record.state.clone(),
                ));
            }

            let handle = record
                .handle
                .clone()
                .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;

            (
                self.backend_for(record.config.backend)?.clone(),
                handle,
            )
        };

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend.snapshot_delete(&handle, &tag, timeout).await?;

        if let Some(store) = &self.store {
            if let Err(e) = store.delete_snapshot(id, &tag).await {
                tracing::warn!(instance_id = %id, tag = %tag, error = %e, "failed to delete snapshot metadata from store");
            }
        }

        Ok(())
    }

    /// Возвращает список снапшотов инстанса.
    ///
    /// Информация берётся из `store` (метаданные) + backend (актуальное
    /// состояние qcow2-файла). Если store недоступен, возвращается пустой
    /// список.
    pub async fn list_snapshots(
        &self,
        id: InstanceId,
    ) -> Result<Vec<SnapshotRecord>, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record.handle.clone();
        let backend = self.backend_for(record.config.backend)?.clone();
        drop(instances);

        let backend_snapshots = if let Some(handle) = handle {
            backend.snapshot_list(&handle).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        if let Some(store) = &self.store {
            let stored = store.load_snapshots(id).await.unwrap_or_default();
            let mut result: Vec<SnapshotRecord> = stored
                .into_iter()
                .map(|s| SnapshotRecord {
                    id: s.id,
                    instance_id: s.instance_id,
                    tag: s.tag,
                    description: s.description,
                    created_at: s.created_at,
                })
                .collect();

            // Merge with backend snapshots (add any that aren't in store yet)
            for bs in &backend_snapshots {
                if !result.iter().any(|r| r.tag == bs.tag) {
                    result.push(SnapshotRecord {
                        id: uuid::Uuid::new_v4(),
                        instance_id: id,
                        tag: bs.tag.clone(),
                        description: None,
                        created_at: bs.created_at.clone().unwrap_or_default(),
                    });
                }
            }

            Ok(result)
        } else {
            Ok(backend_snapshots
                .into_iter()
                .map(|s| SnapshotRecord {
                    id: uuid::Uuid::new_v4(),
                    instance_id: id,
                    tag: s.tag,
                    description: None,
                    created_at: s.created_at.unwrap_or_default(),
                })
                .collect())
        }
    }
}
