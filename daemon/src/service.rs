

use std::pin::Pin;
use std::path::PathBuf;
use std::sync::Arc;

use andler_core::{CloneMode, InstanceConfig};
use andler_rpc::convert;
use andler_rpc::proto::andler_service_server::AndlerService;
use andler_rpc::proto::{
    CloneInstanceRequest, CreateAndroidInstanceRequest, CreateInstanceRequest,
    CreateInstanceResponse, CreateSnapshotRequest, CreateSnapshotResponse, DeleteSnapshotRequest,
    Empty, ExportInstanceDiskRequest, ExportInstanceDiskResponse, GetAndroidBootModeResponse,
    GetInstanceConfigResponse, GuestPackageEntry, InstallGuestAgentRequest, InstanceIdRequest,
    InstanceListEntry, InstanceStatusResponse, ListGuestPackagesResponse, ListInstancesResponse,
    ListSnapshotsResponse, LogLineResponse, RemoveGuestAgentRequest, RemoveInstanceRequest,
    ResourceMetricsResponse, RestoreSnapshotRequest, SetInstanceConfigRequest, SnapshotEntry,
    StopInstanceRequest, SwitchAndroidBootModeRequest, SwitchArmTranslatorRequest,
    UpdateInstanceConfigRequest,
};
use futures_core::Stream;
use futures_util::StreamExt;
use tonic::{Request, Response, Status};

use crate::daemon::{Daemon, DaemonError};
use crate::firmware::OvmfPaths;

pub struct DaemonService {
    daemon: Arc<Daemon>,
    ovmf: OvmfPaths,
}

impl DaemonService {
    pub fn new(daemon: Arc<Daemon>, ovmf: OvmfPaths) -> Self {
        DaemonService { daemon, ovmf }
    }
}


impl From<DaemonError> for Status {
    fn from(err: DaemonError) -> Self {
        match &err {
            DaemonError::InstanceNotFound(_) => Status::not_found(err.to_string()),
            DaemonError::NoBackendRegistered(_) => Status::unimplemented(err.to_string()),
            DaemonError::InvalidTransition(_) => Status::failed_precondition(err.to_string()),
            DaemonError::InstanceNotRemovable(_, _) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::InstanceNotClonable(_, _) => Status::failed_precondition(err.to_string()),
            DaemonError::SharedBaseNotSupportedForLinuxVm(_) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::InstanceHasLiveClones(_, _) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::SnapshotNotFound { .. } => Status::not_found(err.to_string()),
            DaemonError::SnapshotAlreadyExists { .. } => Status::already_exists(err.to_string()),
            DaemonError::SnapshotOperationRequiresRunningInstance(_, _) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::SnapshotLimitExceeded { .. } => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::Backend(andler_core::BackendError::NotImplemented { .. }) => {
                Status::unimplemented(err.to_string())
            }
            DaemonError::Backend(andler_core::BackendError::HandleNotFound(_)) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::Backend(andler_core::BackendError::ProcessNotRunning) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::Disk(andler_disk::DiskError::InsufficientDiskSpace { .. }) => {
                Status::resource_exhausted(err.to_string())
            }
            DaemonError::Backend(_)
            | DaemonError::Disk(_)
            | DaemonError::Io { .. }
            | DaemonError::Restore(_) => Status::internal(err.to_string()),
            DaemonError::Firmware(_) => Status::internal(err.to_string()),
            DaemonError::EmptyInstanceRef => Status::invalid_argument(err.to_string()),
            DaemonError::MalformedInstanceRef(_) => Status::invalid_argument(err.to_string()),
            DaemonError::InstanceRefNotFound(_) => Status::not_found(err.to_string()),
            DaemonError::AmbiguousInstanceId { .. } => Status::invalid_argument(err.to_string()),
            DaemonError::ConfigIdMismatch { .. } => Status::invalid_argument(err.to_string()),
            DaemonError::ConfigKindChanged(_) => Status::invalid_argument(err.to_string()),
            DaemonError::ConfigDiskPathChanged(_) => Status::invalid_argument(err.to_string()),
            DaemonError::GuestAgentUnavailable { .. } => Status::failed_precondition(err.to_string()),
            DaemonError::InvalidConfigKey(_) => Status::invalid_argument(err.to_string()),
            DaemonError::NotAndroid(_) => Status::failed_precondition(err.to_string()),
            DaemonError::InstanceMustBeStopped(_, _) => Status::failed_precondition(err.to_string()),
            DaemonError::MissingOvmfVarsTemplate => Status::invalid_argument(err.to_string()),
        }
    }
}


#[tonic::async_trait]
impl AndlerService for DaemonService {

    type StreamInstanceLogsStream =
        Pin<Box<dyn Stream<Item = Result<LogLineResponse, Status>> + Send + 'static>>;

    type StreamResourceMetricsStream =
        Pin<Box<dyn Stream<Item = Result<ResourceMetricsResponse, Status>> + Send + 'static>>;


    async fn create_instance(
        &self,
        request: Request<CreateInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let mut cfg = InstanceConfig::try_from(request.into_inner())?;

        if cfg.firmware.ovmf_code_path.as_os_str().is_empty() {
            cfg.firmware.ovmf_code_path = self.ovmf.code.clone();
        }

        let ovmf_vars_template = if !cfg.firmware.enable_uefi {
            PathBuf::new()
        } else if cfg.firmware.ovmf_vars_path.as_os_str().is_empty() {
            self.ovmf.vars_template.clone()
        } else {
            cfg.firmware.ovmf_vars_path.clone()
        };

        let id = self
            .daemon
            .create_linux_instance(cfg, andler_core::paths::instances_root(), ovmf_vars_template)
            .await?;
        Ok(Response::new(CreateInstanceResponse {
            instance_id: id.0.to_string(),
        }))
    }

    async fn create_android_instance(
        &self,
        request: Request<CreateAndroidInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let req = request.into_inner();
        let profile_msg = req
            .profile
            .ok_or_else(|| Status::invalid_argument("missing profile"))?;
        let profile = andler_core::AndroidProfile::try_from(profile_msg)?;

        let ovmf_vars_template = if req.ovmf_vars_template.is_empty() {
            self.ovmf.vars_template.clone()
        } else {
            req.ovmf_vars_template.into()
        };

        let base_image_path = if req.base_image_path.is_empty() {
            andler_core::base_image::resolve(&profile)
                .map_err(|e| Status::not_found(e.to_string()))?
        } else {
            req.base_image_path.into()
        };

        let id = self
            .daemon
            .create_android_instance(
                profile,
                req.name,
                base_image_path,
                req.instances_root.into(),
                req.overlay_size_bytes,
                ovmf_vars_template,
                req.linked_overlay,
            )
            .await?;

        Ok(Response::new(CreateInstanceResponse {
            instance_id: id.0.to_string(),
        }))
    }

    async fn start_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        self.daemon.start_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn stop_instance(
        &self,
        request: Request<StopInstanceRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        self.daemon.stop_instance(id, req.graceful).await?;
        Ok(Response::new(Empty {}))
    }

    async fn pause_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        self.daemon.pause_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn resume_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        self.daemon.resume_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_instance_status(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<InstanceStatusResponse>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let status = self.daemon.status(id).await?;
        let (state, error_message) = convert::instance_state_to_proto(&status.state);
        Ok(Response::new(InstanceStatusResponse {
            state: state as i32,
            error_message,
            detail: status.detail.unwrap_or_default(),
        }))
    }


    async fn list_instances(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListInstancesResponse>, Status> {
        let summaries = self.daemon.list_instances().await;
        let instances = summaries
            .into_iter()
            .map(|summary| {
                let (state, _error_message) = convert::instance_state_to_proto(&summary.state);
                InstanceListEntry {
                    instance_id: summary.id.0.to_string(),
                    name: summary.name,
                    state: state as i32,
                }
            })
            .collect();

        Ok(Response::new(ListInstancesResponse { instances }))
    }

    async fn remove_instance(
        &self,
        request: Request<RemoveInstanceRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let id = self.daemon.resolve_instance_id(&request.instance_id).await?;
        self.daemon.remove_instance(id, request.purge).await?;
        Ok(Response::new(Empty {}))
    }


    async fn get_instance_config(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<GetInstanceConfigResponse>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let config = self.daemon.get_instance_config(id).await?;
        Ok(Response::new(config.into()))
    }


    async fn update_instance_config(
        &self,
        request: Request<UpdateInstanceConfigRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_ref).await?;
        let config = convert::update_request_to_instance_config(id, req)?;
        self.daemon.update_instance_config(id, config).await?;
        Ok(Response::new(Empty {}))
    }


    async fn stream_instance_logs(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamInstanceLogsStream>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let inner = self.daemon.stream_instance_logs(id).await?;
        let mapped = inner.map(|line| Ok(LogLineResponse::from(line)));
        Ok(Response::new(Box::pin(mapped)))
    }


    async fn stream_resource_metrics(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamResourceMetricsStream>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let inner = self.daemon.stream_resource_metrics(id).await?;
        let mapped = inner.map(|m| Ok(ResourceMetricsResponse::from(m)));
        Ok(Response::new(Box::pin(mapped)))
    }


    async fn clone_instance(
        &self,
        request: Request<CloneInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let req = request.into_inner();
        let source_id = self.daemon.resolve_instance_id(&req.source_instance_id).await?;
        let mode = CloneMode::try_from(req.mode())?;

        let id = self
            .daemon
            .clone_instance(source_id, req.new_name, req.instances_root.into(), mode)
            .await?;

        Ok(Response::new(CreateInstanceResponse {
            instance_id: id.0.to_string(),
        }))
    }


    async fn export_instance_disk(
        &self,
        request: Request<ExportInstanceDiskRequest>,
    ) -> Result<Response<ExportInstanceDiskResponse>, Status> {
        let req = request.into_inner();
        let source_id = self.daemon.resolve_instance_id(&req.source_instance_id).await?;

        self.daemon
            .export_instance_disk(source_id, req.dest_path.clone().into())
            .await?;

        Ok(Response::new(ExportInstanceDiskResponse {
            dest_path: req.dest_path,
        }))
    }


    async fn create_snapshot(
        &self,
        request: Request<CreateSnapshotRequest>,
    ) -> Result<Response<CreateSnapshotResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        let record = self
            .daemon
            .create_snapshot(
                id,
                req.tag,
                Some(req.description).filter(|s| !s.is_empty()),
                req.timeout_secs,
            )
            .await?;

        Ok(Response::new(CreateSnapshotResponse {
            snapshot_id: record.id.to_string(),
            tag: record.tag,
            created_at: record.created_at,
        }))
    }

    async fn restore_snapshot(
        &self,
        request: Request<RestoreSnapshotRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon.restore_snapshot(id, req.tag, req.timeout_secs).await?;

        Ok(Response::new(Empty {}))
    }

    async fn delete_snapshot(
        &self,
        request: Request<DeleteSnapshotRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon.delete_snapshot(id, req.tag, req.timeout_secs).await?;

        Ok(Response::new(Empty {}))
    }

    async fn list_snapshots(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<ListSnapshotsResponse>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;

        let records = self.daemon.list_snapshots(id).await?;

        let snapshots = records
            .into_iter()
            .map(|r| SnapshotEntry {
                snapshot_id: r.id.to_string(),
                tag: r.tag,
                description: r.description.unwrap_or_default(),
                created_at: r.created_at,
            })
            .collect();

        Ok(Response::new(ListSnapshotsResponse { snapshots }))
    }

    async fn install_guest_agent(
        &self,
        request: Request<InstallGuestAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon.install_guest_agent(id, req.package).await?;

        Ok(Response::new(Empty {}))
    }

    async fn remove_guest_agent(
        &self,
        request: Request<RemoveGuestAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon.remove_guest_agent(id, req.package).await?;

        Ok(Response::new(Empty {}))
    }

    async fn list_guest_packages(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<ListGuestPackagesResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        let packages = self.daemon.list_guest_packages(id).await?;

        let entries = packages
            .into_iter()
            .map(|(name, description, status)| GuestPackageEntry {
                name,
                description,
                status,
            })
            .collect();

        Ok(Response::new(ListGuestPackagesResponse { packages: entries }))
    }

    async fn switch_arm_translator(
        &self,
        request: Request<SwitchArmTranslatorRequest>,
    ) -> Result<Response<Empty>, Status> {
        let cmd = convert::SwitchArmTranslatorCmd::try_from(request.into_inner())?;
        let id = self.daemon.resolve_instance_id(&cmd.instance_ref).await?;
        self.daemon
            .switch_arm_translator(id, cmd.translator, cmd.translator_dir)
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn set_instance_config(
        &self,
        request: Request<SetInstanceConfigRequest>,
    ) -> Result<Response<Empty>, Status> {
        let cmd = convert::SetInstanceConfigCmd::try_from(request.into_inner())?;
        let id = self.daemon.resolve_instance_id(&cmd.instance_ref).await?;
        self.daemon
            .set_instance_config(id, &cmd.key, &cmd.value)
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn switch_android_boot_mode(
        &self,
        request: Request<SwitchAndroidBootModeRequest>,
    ) -> Result<Response<Empty>, Status> {
        let cmd = convert::SwitchAndroidBootModeCmd::try_from(request.into_inner())?;
        let id = self.daemon.resolve_instance_id(&cmd.instance_ref).await?;
        self.daemon.switch_android_boot_mode(id, cmd.mode).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_android_boot_mode(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<GetAndroidBootModeResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        let mode = self.daemon.get_android_boot_mode(id).await?;
        Ok(Response::new(GetAndroidBootModeResponse {
            mode: andler_rpc::proto::AndroidBootMode::from(mode) as i32,
        }))
    }

}
