use std::path::PathBuf;

use super::error::DaemonError;
use super::ops::OpProgress;
use super::supervisor::{IdOpAccept, IdOpRunner, OpAccept, OpRunner};
use super::types::{write_instance_toml, InstanceDirGuard};
use super::Daemon;
use andler_core::{
    CloneMode, InstanceConfig, InstanceId, InstanceKind, InstanceState, Operation, OperationKind,
    OperationState,
};

/// Ends a failed operation: records the failure on the progress handle and
/// hands the same error back to the runner's caller.
fn fail_op<T>(progress: &mut OpProgress, err: DaemonError) -> Result<T, DaemonError> {
    progress.finish(Err(err.to_string()));
    Err(err)
}

impl Daemon {
    /// Clones a terminal instance under `mode` as a supervisor operation on
    /// the source: cancellable between the steps, progress on the event bus,
    /// and a same-key retry joins the clone already running instead of
    /// creating a second instance.
    pub async fn clone_instance(
        &self,
        source_id: InstanceId,
        new_name: String,
        instances_root: PathBuf,
        mode: CloneMode,
        idempotency_token: Option<String>,
    ) -> Result<InstanceId, DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;
        let handle = self.handle_for(source_id).await?;

        if mode == CloneMode::SharedBase
            && !matches!(source_config.kind, InstanceKind::AndroidVm { .. })
        {
            return Err(DaemonError::SharedBaseNotSupportedForLinuxVm(source_id));
        }

        let new_id = InstanceId::new();
        let instance_dir = instances_root.join(new_id.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());
        let key = Self::clone_key(source_id, &new_name, mode, idempotency_token);
        let phases = mode.phases();
        let op_id = format!("clone-{new_id}");

        let run: IdOpRunner = {
            let supervisors = self.supervisors.clone();
            let backends = self.backends.clone();
            let events = self.event_sender();
            let source = source_config.clone();
            let new_name = new_name.clone();
            let instance_dir = instance_dir.clone();
            let op_id = op_id.clone();
            Box::new(move |mut progress| {
                Box::pin(async move {
                    progress.enter_phase("validate");
                    if progress.is_cancelled() {
                        return fail_op(
                            &mut progress,
                            DaemonError::OperationCancelled(op_id.clone()),
                        );
                    }
                    let mut new_config = source.clone();
                    new_config.id = new_id;
                    new_config.name = new_name;
                    progress.set_progress(1.0);

                    progress.enter_phase("clone-disk");
                    let new_disk_path = instance_dir.join("disk.qcow2");
                    let new_ovmf_vars_path = instance_dir.join("VARS.fd");
                    if let Err(source) = andler_core::paths::ensure_private_dir(&instance_dir).await
                    {
                        return fail_op(
                            &mut progress,
                            DaemonError::Io {
                                path: instance_dir.clone(),
                                source,
                            },
                        );
                    }
                    if let Err(source) =
                        tokio::fs::copy(&source.firmware.ovmf_vars_path, &new_ovmf_vars_path).await
                    {
                        return fail_op(
                            &mut progress,
                            DaemonError::Io {
                                path: new_ovmf_vars_path.clone(),
                                source,
                            },
                        );
                    }
                    let cloned = match mode {
                        CloneMode::Linked => {
                            andler_disk::clone::linked_clone(
                                &source.disk.path,
                                &new_disk_path,
                                source.disk.size_bytes,
                            )
                            .await
                        }
                        CloneMode::FullStandalone => {
                            andler_disk::clone::full_standalone_clone(
                                &source.disk.path,
                                &new_disk_path,
                            )
                            .await
                        }
                        CloneMode::SharedBase => {
                            let Some(base_image) = source.disk.base_image.clone() else {
                                return fail_op(
                                    &mut progress,
                                    DaemonError::Io {
                                        path: source.disk.path.clone(),
                                        source: std::io::Error::new(
                                            std::io::ErrorKind::InvalidInput,
                                            "SharedBase clone requires a source disk with base_image set",
                                        ),
                                    },
                                );
                            };
                            andler_disk::clone::shared_base_clone(
                                &source.disk.path,
                                &new_disk_path,
                                &base_image,
                            )
                            .await
                        }
                    };
                    let cloned = match cloned {
                        Ok(cloned) => cloned,
                        Err(err) => return fail_op(&mut progress, DaemonError::from(err)),
                    };
                    if progress.is_cancelled() {
                        return fail_op(
                            &mut progress,
                            DaemonError::OperationCancelled(op_id.clone()),
                        );
                    }
                    progress.set_progress(1.0);

                    progress.enter_phase("register");
                    new_config.disk.path = cloned.disk_path;
                    new_config.disk.base_image = cloned.backing_file;
                    new_config.firmware.ovmf_vars_path = new_ovmf_vars_path;
                    write_instance_toml(&instance_dir, &new_config).await;
                    if let Err(err) = Daemon::register_instance(
                        &supervisors,
                        &backends,
                        &events,
                        new_config.clone(),
                        instance_dir.clone(),
                    )
                    .await
                    {
                        return fail_op(&mut progress, err);
                    }
                    progress.set_progress(1.0);
                    progress.finish(Ok(()));
                    Ok(new_id)
                })
            })
        };

        match handle
            .run_id_operation(
                Operation {
                    op_id: op_id.clone(),
                    instance_id: source_id,
                    kind: OperationKind::Clone,
                    phases,
                    progress: 0.0,
                    current_phase: None,
                    state: OperationState::Queued,
                    error: None,
                },
                key,
                new_id,
                run,
            )
            .await?
        {
            IdOpAccept::Started { done } => {
                let id = done
                    .await
                    .map_err(|_| DaemonError::InstanceSupervisorGone(source_id))??;
                dir_guard.disarm();
                Ok(id)
            }
            IdOpAccept::Joined { op_id, new_id } => {
                // The joiner created nothing: its own candidate id and
                // directory are abandoned, the first request owns the result.
                dir_guard.disarm();
                tracing::warn!(
                    instance_id = %source_id,
                    joined = %op_id,
                    new_id = %new_id,
                    "clone joined an already-running clone for the same source, name and mode"
                );
                Ok(new_id)
            }
        }
    }

    /// Idempotency join key for a clone: the source, name and mode triple
    /// that identifies the instance being created.
    fn clone_key(
        source_id: InstanceId,
        new_name: &str,
        mode: CloneMode,
        idempotency_token: Option<String>,
    ) -> Option<String> {
        idempotency_token.or_else(|| Some(format!("clone:{source_id}:{new_name}:{mode:?}")))
    }

    /// Exports a terminal instance's disk to `dest_path` as a supervisor
    /// operation on the source: cancellable, progress on the event bus, and
    /// a same-key retry joins the export already running to that path.
    pub async fn export_instance_disk(
        &self,
        source_id: InstanceId,
        dest_path: PathBuf,
        idempotency_token: Option<String>,
    ) -> Result<(), DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;
        let handle = self.handle_for(source_id).await?;
        let op_id = format!("export-{source_id}");
        let key = Self::export_key(source_id, &dest_path, idempotency_token);
        let phases: Vec<(String, f32)> =
            vec![("validate".to_string(), 0.1), ("convert".to_string(), 0.9)];

        let run: OpRunner = {
            let source_disk = source_config.disk.path.clone();
            let dest_path = dest_path.clone();
            let op_id = op_id.clone();
            Box::new(move |mut progress| {
                Box::pin(async move {
                    progress.enter_phase("validate");
                    if progress.is_cancelled() {
                        return fail_op(
                            &mut progress,
                            DaemonError::OperationCancelled(op_id.clone()),
                        );
                    }
                    progress.set_progress(1.0);

                    progress.enter_phase("convert");
                    if let Err(err) =
                        andler_disk::clone::full_standalone_clone(&source_disk, &dest_path).await
                    {
                        return fail_op(&mut progress, DaemonError::from(err));
                    }
                    if progress.is_cancelled() {
                        return fail_op(
                            &mut progress,
                            DaemonError::OperationCancelled(op_id.clone()),
                        );
                    }
                    progress.set_progress(1.0);
                    progress.finish(Ok(()));
                    Ok(())
                })
            })
        };

        match handle
            .run_operation(
                Operation {
                    op_id: op_id.clone(),
                    instance_id: source_id,
                    kind: OperationKind::Export,
                    phases,
                    progress: 0.0,
                    current_phase: None,
                    state: OperationState::Queued,
                    error: None,
                },
                key,
                run,
            )
            .await?
        {
            OpAccept::Started { done } => done
                .await
                .map_err(|_| DaemonError::InstanceSupervisorGone(source_id))?,
            OpAccept::Joined { op_id } => {
                tracing::warn!(
                    instance_id = %source_id,
                    joined = %op_id,
                    "disk export joined an already-running export to the same destination"
                );
                Ok(())
            }
        }
    }

    /// Idempotency join key for a disk export: source plus destination.
    fn export_key(
        source_id: InstanceId,
        dest_path: &std::path::Path,
        idempotency_token: Option<String>,
    ) -> Option<String> {
        idempotency_token.or_else(|| Some(format!("export:{source_id}:{}", dest_path.display())))
    }

    pub async fn terminal_clonable_instance_config(
        &self,
        id: InstanceId,
    ) -> Result<InstanceConfig, DaemonError> {
        let handle = self.handle_for(id).await?;
        let (state, config) = (handle.state(), handle.config());

        let clonable = matches!(
            state,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        );
        if !clonable {
            return Err(DaemonError::InstanceNotClonable(id, state));
        }

        Ok(config)
    }

    pub async fn find_live_clones(&self, id: InstanceId) -> Result<Vec<InstanceId>, DaemonError> {
        let supervisors = self.supervisors.read().await;
        let target_disk_path = supervisors
            .get(&id)
            .map(|handle| handle.config().disk.path.clone())
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let clones = supervisors
            .iter()
            .filter(|(other_id, handle)| {
                **other_id != id
                    && handle.config().disk.base_image.as_deref()
                        == Some(target_disk_path.as_path())
            })
            .map(|(other_id, _)| *other_id)
            .collect();

        Ok(clones)
    }

    /// Finds instances other than `id` whose disk chain (walked file by file
    /// through qcow2 backing references) contains `layer_path`. A linked
    /// clone derives from the source's disk, so after a snapshot renamed the
    /// source head into a layer, the clone's chain reaches that layer —
    /// deleting the layer would orphan the clone.
    pub async fn find_chain_consumers(
        &self,
        id: InstanceId,
        layer_path: &std::path::Path,
    ) -> Result<Vec<InstanceId>, DaemonError> {
        let supervisors = self.supervisors.read().await;
        let mut consumers = Vec::new();
        for (other_id, handle) in supervisors.iter() {
            if *other_id == id {
                continue;
            }
            let other_disk = handle.config().disk.path.clone();
            let Ok(chain) = andler_disk::qcow2::chain_from_head(&other_disk).await else {
                continue;
            };
            if chain.iter().any(|path| path == layer_path) {
                consumers.push(*other_id);
            }
        }
        Ok(consumers)
    }
}
