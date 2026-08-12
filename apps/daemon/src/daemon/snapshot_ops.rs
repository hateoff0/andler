use std::path::PathBuf;

use super::disk_chain;
use super::error::DaemonError;
use super::types::SnapshotRecord;
use super::Daemon;
use andler_core::{DiskFormat, InstanceId, InstanceState};

const MAX_SNAPSHOTS_PER_INSTANCE: usize = 20;

impl Daemon {
    /// Creates a live external snapshot: the VM's disk graph is switched to
    /// a new overlay via QMP, then the rename pair turns the previous active
    /// file into a snapshot layer and the overlay into the new active file.
    /// Metadata is written last; a crash anywhere before that leaves a
    /// layer without metadata, which startup reconciliation recovers.
    pub async fn create_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        description: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<SnapshotRecord, DaemonError> {
        let (backend, handle) = self.with_running_instance(id).await?;

        let (disk_path, disk_format, configured_size, memory_size_bytes) = {
            let handle = self.handle_for(id).await?;
            let config = handle.config();
            (
                config.disk.path.clone(),
                config.disk.format,
                config.disk.size_bytes,
                config.memory.size_bytes,
            )
        };

        if disk_format != DiskFormat::Qcow2 {
            return Err(DaemonError::SnapshotRequiresQcow2 {
                instance_id: id,
                format: format!("{disk_format:?}"),
            });
        }

        if let Some(store) = &self.store {
            if store
                .get_snapshot(id, &tag)
                .await
                .map_err(DaemonError::Store)?
                .is_some()
            {
                return Err(DaemonError::SnapshotAlreadyExists {
                    instance_id: id,
                    tag,
                });
            }
        }

        let mut current = 0usize;
        if let Some(store) = &self.store {
            current += store
                .load_snapshots(id)
                .await
                .map_err(DaemonError::Store)?
                .len();
        }
        if let Ok(existing) = backend.snapshot_list(&handle).await {
            current += existing.len();
        }
        check_snapshot_limit(id, current, MAX_SNAPSHOTS_PER_INSTANCE)?;

        andler_disk::check_available_space(&disk_path, memory_size_bytes)?;

        let actual_size = andler_disk::qcow2::virtual_size_bytes(&disk_path)
            .await
            .ok()
            .unwrap_or(0);
        let virtual_size = actual_size.max(configured_size);

        let instance_dir = disk_chain::instance_dir_of(id).await;
        let snapshots_dir = disk_chain::snapshots_dir(&instance_dir);
        tokio::fs::create_dir_all(&snapshots_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: snapshots_dir.clone(),
                source,
            })?;

        let uuid = uuid::Uuid::new_v4();
        let layer_rel = format!("disk.snapshots/{uuid}.qcow2");
        let layer_path = snapshots_dir.join(format!("{uuid}.qcow2"));
        let tmp_path = snapshots_dir.join(format!(".tmp-{uuid}.qcow2"));

        // Overlay whose backing reference already points at the future layer
        // path. The file does not exist yet: it only becomes resolvable once
        // the rename pair moves the current head there.
        andler_disk::qcow2::create(&tmp_path, virtual_size)
            .await
            .map_err(DaemonError::Disk)?;
        andler_disk::qcow2::rebase_unchanged(&tmp_path, &layer_path)
            .await
            .map_err(DaemonError::Disk)?;

        let timeout = timeout_secs.map(std::time::Duration::from_secs);
        if let Err(err) = backend.snapshot(&handle, &tmp_path, timeout).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(DaemonError::Backend(err));
        }

        if let Err(source) = tokio::fs::rename(&disk_path, &layer_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(DaemonError::Io {
                path: disk_path.clone(),
                source,
            });
        }
        if let Err(source) = tokio::fs::rename(&tmp_path, &disk_path).await {
            let _ = tokio::fs::rename(&layer_path, &disk_path).await;
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(DaemonError::Io {
                path: tmp_path,
                source,
            });
        }

        let parent_id = match &self.store {
            Some(store) => disk_chain::main_chain_head(store, id)
                .await?
                .map(|record| record.id),
            None => None,
        };

        let record = SnapshotRecord {
            id: uuid,
            instance_id: id,
            tag: tag.clone(),
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
            layer_path: Some(layer_rel),
            parent_id,
            branch: None,
        };

        if let Some(store) = &self.store {
            let stored = andler_store::StoredSnapshot {
                id: record.id,
                instance_id: record.instance_id,
                tag: record.tag.clone(),
                description: record.description.clone(),
                created_at: record.created_at.clone(),
                layer_path: record.layer_path.clone(),
                parent_id: record.parent_id,
                branch: None,
            };
            if let Err(e) = store.save_snapshot(&stored).await {
                tracing::warn!(instance_id = %id, tag = %tag, error = %e, "failed to persist snapshot metadata; daemon restart will recover it");
            }
        }

        Ok(record)
    }

    /// Restores a snapshot with the VM stopped. Default (discard) mode
    /// deletes every layer newer than the target and rebuilds the active
    /// disk on top of it — the linear history continues from the target.
    /// `--branch` mode archives the whole current chain into a branch and
    /// rebuilds the active disk on top of the target without deleting
    /// anything; if the target lives on an archived branch, that branch
    /// becomes the main branch again.
    pub async fn restore_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        branch: bool,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let (disk_path, configured_size) = {
            let config = handle.config();
            (config.disk.path.clone(), config.disk.size_bytes)
        };
        let state = handle.state();
        ensure_snapshot_restore_allowed(id, &state)?;

        let Some(store) = &self.store else {
            return andler_disk::qcow2::restore_internal_snapshot(&disk_path, &tag)
                .await
                .map_err(DaemonError::Disk);
        };

        let target = store
            .get_snapshot(id, &tag)
            .await
            .map_err(DaemonError::Store)?
            .ok_or_else(|| DaemonError::SnapshotNotFound {
                instance_id: id,
                tag: tag.clone(),
            })?;

        let Some(layer_rel) = &target.layer_path else {
            return Err(DaemonError::SnapshotInternalNotRestorable {
                instance_id: id,
                tag,
            });
        };

        let instance_dir = disk_chain::instance_dir_of(id).await;
        let target_path = instance_dir.join(layer_rel);
        if !target_path.exists() {
            return Err(DaemonError::SnapshotLayerMissing {
                instance_id: id,
                path: target_path,
            });
        }

        let clones = self.find_live_clones(id).await?;
        if !clones.is_empty() {
            return Err(DaemonError::RestoreWouldBreakClones {
                instance_id: id,
                tag: tag.clone(),
                clones,
            });
        }

        let records = store.load_snapshots(id).await.map_err(DaemonError::Store)?;

        if !branch {
            if let Some(branch_name) = &target.branch {
                return Err(DaemonError::RestoreTargetOnArchivedBranch {
                    instance_id: id,
                    tag: tag.clone(),
                    branch: branch_name.clone(),
                });
            }

            for child in disk_chain::descendants_in_branch(&records, target.id, None) {
                let child_rel = child.layer_path.as_deref().unwrap_or_default();
                let child_path = instance_dir.join(child_rel);
                tokio::fs::remove_file(&child_path)
                    .await
                    .map_err(|source| DaemonError::Io {
                        path: child_path.clone(),
                        source,
                    })?;
                store
                    .delete_snapshot_by_id(id, child.id)
                    .await
                    .map_err(DaemonError::Store)?;
            }

            tokio::fs::remove_file(&disk_path)
                .await
                .map_err(|source| DaemonError::Io {
                    path: disk_path.clone(),
                    source,
                })?;
        } else {
            let branch_name = format!("branch-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));

            let archived_head = disk_chain::main_chain_head(store, id).await?;
            let arch_uuid = uuid::Uuid::new_v4();
            let arch_rel = format!("disk.snapshots/{arch_uuid}.qcow2");
            let arch_path =
                disk_chain::snapshots_dir(&instance_dir).join(format!("{arch_uuid}.qcow2"));

            tokio::fs::rename(&disk_path, &arch_path)
                .await
                .map_err(|source| DaemonError::Io {
                    path: disk_path.clone(),
                    source,
                })?;

            for record in &records {
                if record.layer_path.is_some() && record.branch.is_none() {
                    store
                        .set_snapshot_branch(id, record.id, Some(branch_name.clone()))
                        .await
                        .map_err(DaemonError::Store)?;
                }
            }

            let archived = andler_store::StoredSnapshot {
                id: arch_uuid,
                instance_id: id,
                tag: format!("pre-{branch_name}"),
                description: Some(format!(
                    "active disk archived when branching off snapshot {tag:?}"
                )),
                created_at: chrono::Utc::now().to_rfc3339(),
                layer_path: Some(arch_rel),
                parent_id: archived_head.map(|record| record.id),
                branch: Some(branch_name),
            };
            store
                .save_snapshot(&archived)
                .await
                .map_err(DaemonError::Store)?;

            if let Some(target_branch) = &target.branch {
                for ancestor in disk_chain::ancestors(&records, target.id) {
                    if ancestor.branch.as_deref() == Some(target_branch.as_str()) {
                        store
                            .set_snapshot_branch(id, ancestor.id, None)
                            .await
                            .map_err(DaemonError::Store)?;
                    }
                }
            }
        }

        let actual_size = andler_disk::qcow2::virtual_size_bytes(&disk_path)
            .await
            .ok()
            .unwrap_or(0);
        let virtual_size = actual_size.max(configured_size);

        andler_disk::qcow2::create_overlay(&disk_path, &target_path, virtual_size)
            .await
            .map_err(DaemonError::Disk)?;

        Ok(())
    }

    /// Deletes a snapshot layer with the VM stopped. The layer's data is
    /// committed into its parent (`qemu-img commit`), its children are
    /// re-pointed at the parent (`qemu-img rebase -u`), then the layer file
    /// and its metadata entry are removed. Legacy internal snapshots are
    /// deleted with `qemu-img snapshot -d`.
    pub async fn delete_snapshot(
        &self,
        id: InstanceId,
        tag: String,
        _timeout_secs: Option<u64>,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        let state = handle.state();
        ensure_snapshot_restore_allowed(id, &state)?;
        let disk_path = handle.config().disk.path.clone();

        let Some(store) = &self.store else {
            return andler_disk::qcow2::delete_internal_snapshot(&disk_path, &tag)
                .await
                .map_err(DaemonError::Disk);
        };

        let target = store
            .get_snapshot(id, &tag)
            .await
            .map_err(DaemonError::Store)?
            .ok_or_else(|| DaemonError::SnapshotNotFound {
                instance_id: id,
                tag: tag.clone(),
            })?;

        let Some(layer_rel) = &target.layer_path else {
            andler_disk::qcow2::delete_internal_snapshot(&disk_path, &tag)
                .await
                .map_err(DaemonError::Disk)?;
            store
                .delete_snapshot(id, &tag)
                .await
                .map_err(DaemonError::Store)?;
            return Ok(());
        };

        let instance_dir = disk_chain::instance_dir_of(id).await;
        let target_path = instance_dir.join(layer_rel);
        if !target_path.exists() {
            return Err(DaemonError::SnapshotLayerMissing {
                instance_id: id,
                path: target_path,
            });
        }

        let Some(parent_id) = target.parent_id else {
            return Err(DaemonError::CannotDeleteBaseLayer {
                instance_id: id,
                tag: tag.clone(),
            });
        };
        let records = store.load_snapshots(id).await.map_err(DaemonError::Store)?;
        let parent = records
            .iter()
            .find(|record| record.id == parent_id)
            .ok_or_else(|| DaemonError::SnapshotLayerMissing {
                instance_id: id,
                path: PathBuf::from("parent layer record missing"),
            })?;
        let parent_path = instance_dir.join(parent.layer_path.as_deref().unwrap_or_default());
        if !parent_path.exists() {
            return Err(DaemonError::SnapshotLayerMissing {
                instance_id: id,
                path: parent_path,
            });
        }

        let consumers = self.find_chain_consumers(id, &target_path).await?;
        if !consumers.is_empty() {
            return Err(DaemonError::DeleteWouldBreakClones {
                instance_id: id,
                tag: tag.clone(),
                clones: consumers,
            });
        }

        andler_disk::qcow2::commit_layer(&target_path)
            .await
            .map_err(DaemonError::Disk)?;

        let children = disk_chain::direct_children(&records, target.id);
        for child in children {
            let child_path = instance_dir.join(child.layer_path.as_deref().unwrap_or_default());
            andler_disk::qcow2::rebase_unchanged(&child_path, &parent_path)
                .await
                .map_err(DaemonError::Disk)?;
        }

        if let Ok(head_info) = andler_disk::qcow2::info(&disk_path).await {
            let target_str = target_path.to_string_lossy().into_owned();
            if head_info.backing_file.as_deref() == Some(target_str.as_str()) {
                andler_disk::qcow2::rebase_unchanged(&disk_path, &parent_path)
                    .await
                    .map_err(DaemonError::Disk)?;
            }
        }

        tokio::fs::remove_file(&target_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: target_path.clone(),
                source,
            })?;
        store
            .delete_snapshot_by_id(id, target.id)
            .await
            .map_err(DaemonError::Store)?;

        Ok(())
    }

    pub async fn list_snapshots(&self, id: InstanceId) -> Result<Vec<SnapshotRecord>, DaemonError> {
        let handle = self.handle_for(id).await?;
        let backend_handle = handle.backend_handle();
        let backend = self.backend_for(handle.config().backend)?.clone();

        let backend_snapshots = if let Some(backend_handle) = backend_handle {
            backend
                .snapshot_list(&backend_handle)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if let Some(store) = &self.store {
            let stored = store.load_snapshots(id).await.map_err(DaemonError::Store)?;
            let mut result: Vec<SnapshotRecord> = stored
                .into_iter()
                .map(|s| SnapshotRecord {
                    id: s.id,
                    instance_id: s.instance_id,
                    tag: s.tag,
                    description: s.description,
                    created_at: s.created_at,
                    layer_path: s.layer_path,
                    parent_id: s.parent_id,
                    branch: s.branch,
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
                        layer_path: None,
                        parent_id: None,
                        branch: None,
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
                    layer_path: None,
                    parent_id: None,
                    branch: None,
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

fn ensure_snapshot_restore_allowed(
    id: InstanceId,
    state: &InstanceState,
) -> Result<(), DaemonError> {
    if matches!(state, InstanceState::Running | InstanceState::Paused) {
        return Err(DaemonError::InstanceMustBeStopped(id, state.clone()));
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
        assert!(check_snapshot_limit(
            id,
            MAX_SNAPSHOTS_PER_INSTANCE - 1,
            MAX_SNAPSHOTS_PER_INSTANCE
        )
        .is_ok());
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
    fn restore_not_allowed_while_running() {
        let id = InstanceId::new();
        assert!(ensure_snapshot_restore_allowed(id, &InstanceState::Stopped).is_ok());
        assert!(ensure_snapshot_restore_allowed(id, &InstanceState::Created).is_ok());
        assert!(matches!(
            ensure_snapshot_restore_allowed(id, &InstanceState::Running),
            Err(DaemonError::InstanceMustBeStopped(_, _))
        ));
    }
}
