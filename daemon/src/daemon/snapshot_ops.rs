use super::Daemon;
use super::error::DaemonError;
use super::types::{SnapshotRecord};
use andler_core::{BackendError, InstanceId, InstanceState};

/// Максимум internal-снапшотов на один инстанс (см. документацию
/// `DaemonError::SnapshotLimitExceeded` за тем, почему лимит вообще
/// нужен). 20 — произвольный, но не случайный выбор: достаточно для
/// типичного цикла разработки/тестирования ("снапшот перед каждым
/// рискованным шагом", несколько раз в день, несколько дней подряд), но
/// не настолько много, чтобы qcow2-файл рос неконтролируемо между
/// ручными чистками. Не настраивается через `InstanceConfig` — это
/// защитный лимит самого ANDLER, а не часть декларативной конфигурации
/// инстанса.
const MAX_SNAPSHOTS_PER_INSTANCE: usize = 20;

impl Daemon {
    /// Создаёт снапшот инстанса с указанным тегом.
    ///
    /// Тег — произвольная строка, уникальная в пределах инстанса.
    ///
    /// Алгоритм:
    /// 1. Проверить FSM: `Running`/`Paused`
    /// 2. Проверить лимит: не больше `MAX_SNAPSHOTS_PER_INSTANCE` уже существующих
    /// 3. Вызвать `backend.snapshot(handle, tag)` (async job через QMP)
    /// 4. Сохранить метаданные в `store`
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

        // Считается по backend'у (реальные снапшоты внутри qcow2), не по
        // `store` — `store` может отставать/быть недоступен (см.
        // `list_snapshots`), а лимит должен опираться на то, что реально
        // будет храниться в файле диска. Если сам список получить не
        // удалось, лимит не проверяется (тот же принцип толерантности,
        // что и в `list_snapshots::unwrap_or_default` — не блокировать
        // создание снапшота из-за временной невозможности его
        // пересчитать, настоящую ошибку в этом случае вернёт сам
        // `backend.snapshot()` ниже).
        if let Ok(existing) = backend.snapshot_list(&handle).await {
            check_snapshot_limit(id, existing.len(), MAX_SNAPSHOTS_PER_INSTANCE)?;
        }

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

/// Чистая часть проверки лимита снапшотов — вынесена из `create_snapshot`
/// отдельно, потому что сама `create_snapshot` требует живого backend'а с
/// уже запущенным QEMU-процессом (реальный `qmp`-хэндшейк для
/// `snapshot_list`), которого нет в юнит-тестах этого крейта; так
/// проверяется хотя бы сама логика "сколько есть vs сколько можно", а не
/// путь целиком.
fn check_snapshot_limit(
    instance_id: InstanceId,
    current: usize,
    limit: usize,
) -> Result<(), DaemonError> {
    if current >= limit {
        return Err(DaemonError::SnapshotLimitExceeded {
            instance_id,
            current,
            limit,
        });
    }
    Ok(())
}

#[cfg(test)]
mod snapshot_limit_tests {
    use super::*;

    #[test]
    fn below_limit_is_allowed() {
        let id = InstanceId::new();
        assert!(check_snapshot_limit(id, 0, MAX_SNAPSHOTS_PER_INSTANCE).is_ok());
        assert!(check_snapshot_limit(id, MAX_SNAPSHOTS_PER_INSTANCE - 1, MAX_SNAPSHOTS_PER_INSTANCE).is_ok());
    }

    #[test]
    fn at_limit_is_rejected() {
        let id = InstanceId::new();
        let err = check_snapshot_limit(id, MAX_SNAPSHOTS_PER_INSTANCE, MAX_SNAPSHOTS_PER_INSTANCE)
            .unwrap_err();
        match err {
            DaemonError::SnapshotLimitExceeded {
                instance_id,
                current,
                limit,
            } => {
                assert_eq!(instance_id, id);
                assert_eq!(current, MAX_SNAPSHOTS_PER_INSTANCE);
                assert_eq!(limit, MAX_SNAPSHOTS_PER_INSTANCE);
            }
            other => panic!("expected SnapshotLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn above_limit_is_rejected() {
        // Defensive: shouldn't happen in practice (limit is checked
        // before every create), but a stale/racy count above the limit
        // must still be rejected, not treated as "not quite at the
        // limit yet".
        let id = InstanceId::new();
        assert!(check_snapshot_limit(id, MAX_SNAPSHOTS_PER_INSTANCE + 1, MAX_SNAPSHOTS_PER_INSTANCE).is_err());
    }
}
