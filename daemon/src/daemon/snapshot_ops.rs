use super::Daemon;
use super::error::DaemonError;
use super::types::{SnapshotRecord};
use andler_core::InstanceId;


const MAX_SNAPSHOTS_PER_INSTANCE: usize = 20;

impl Daemon {

    pub async fn create_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        description: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<SnapshotRecord, DaemonError> {
        let (backend, handle) = self.with_running_instance(id).await?;

        let (disk_path, memory_size_bytes) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.config.disk.path.clone(),
                record.config.memory.size_bytes,
            )
        };

        if let Ok(existing) = backend.snapshot_list(&handle).await {
            check_snapshot_limit(id, existing.len(), MAX_SNAPSHOTS_PER_INSTANCE)?;
        }

        andler_disk::check_available_space(&disk_path, memory_size_bytes)?;

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


    pub async fn restore_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        timeout_secs: Option<u64>,
    ) -> Result<(), DaemonError> {
        let (backend, handle) = self.with_running_instance(id).await?;

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend.snapshot_restore(&handle, &tag, timeout).await?;
        Ok(())
    }


    pub async fn delete_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        timeout_secs: Option<u64>,
    ) -> Result<(), DaemonError> {
        let (backend, handle) = self.with_running_instance(id).await?;

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        backend.snapshot_delete(&handle, &tag, timeout).await?;

        if let Some(store) = &self.store {
            if let Err(e) = store.delete_snapshot(id, &tag).await {
                tracing::warn!(instance_id = %id, tag = %tag, error = %e, "failed to delete snapshot metadata from store");
            }
        }

        Ok(())
    }


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
        let id = InstanceId::new();
        assert!(check_snapshot_limit(id, MAX_SNAPSHOTS_PER_INSTANCE + 1, MAX_SNAPSHOTS_PER_INSTANCE).is_err());
    }
}
