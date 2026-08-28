use std::path::PathBuf;
use std::sync::Arc;

use andler_core::EventKind;
use andler_core::{CloneMode, InstanceConfig};
use andler_rpc::convert;
use andler_rpc::proto::andler_service_server::AndlerService;
use andler_rpc::proto::{
    AttachDiskRequest, AttachDiskResponse, AttachNetworkRequest, AttachNetworkResponse,
    CloneInstanceRequest, CodeCount, ConfigKeyDiff, CreateAndroidInstanceRequest,
    CreateInstanceRequest, CreateInstanceResponse, CreateSnapshotRequest, CreateSnapshotResponse,
    DaemonEventMessage, DaemonLogLine, DaemonLogsRequest, DaemonMetricsResponse,
    DeleteSnapshotRequest, DetachDiskRequest, DetachNetworkRequest, Empty, EventStreamRequest,
    ExecCommandRequest, ExecCommandResponse, ExportInstanceDiskRequest, ExportInstanceDiskResponse,
    GetAndroidBootModeResponse, GetConfigStatusResponse, GetInstanceConfigResponse,
    GuestPackageEntry, GuestProvisionRequest, InstallGuestAgentRequest, InstanceIdRequest,
    InstanceListEntry, InstanceStatusResponse, ListGuestPackagesResponse, ListInstancesResponse,
    ListSnapshotsResponse, LogLineResponse, MethodLatency, OpCancelRequest, OpListResponse,
    OperationInfo, OperationPhase, RemoveGuestAgentRequest, RemoveInstanceRequest,
    ResourceMetricsResponse, RestoreSnapshotRequest, SetInstanceConfigRequest, SnapshotEntry,
    StopInstanceRequest, SwitchAndroidBootModeRequest, SwitchArmTranslatorRequest,
    UpdateInstanceConfigRequest, VersionResponse,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use tonic::{Request, Response, Status};

use crate::daemon::{Daemon, DaemonError, ErrorKind};
use crate::firmware::OvmfPaths;

pub struct DaemonService {
    daemon: Arc<Daemon>,
    ovmf: OvmfPaths,
    log_ring: std::sync::Arc<crate::log_ring::LogRing>,
    metrics: std::sync::Arc<crate::metrics::DaemonMetrics>,
}

impl DaemonService {
    pub fn new(
        daemon: Arc<Daemon>,
        ovmf: OvmfPaths,
        log_ring: std::sync::Arc<crate::log_ring::LogRing>,
        metrics: std::sync::Arc<crate::metrics::DaemonMetrics>,
    ) -> Self {
        DaemonService {
            daemon,
            ovmf,
            log_ring,
            metrics,
        }
    }
}

fn status_message(err: &DaemonError) -> String {
    // The gRPC "grpc-message" header must not contain control characters other
    // than HTAB: h2 rejects them and clients surface "h2 protocol error"
    // instead of the real error (e.g. multi-line package-manager stderr).
    // Long messages (multi-KB package-manager stderr) also trip the h2 client
    // (PROTOCOL_ERROR on oversized trailers), so truncate the details.
    const MAX_MESSAGE_CHARS: usize = 384;
    let s = err.to_string();
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
        .collect();
    cleaned.chars().take(MAX_MESSAGE_CHARS).collect()
}

impl From<DaemonError> for Status {
    fn from(err: DaemonError) -> Self {
        let msg = status_message(&err);
        match err.kind() {
            ErrorKind::InvalidArgument => Status::invalid_argument(msg.clone()),
            ErrorKind::NotFound => Status::not_found(msg.clone()),
            ErrorKind::AlreadyExists => Status::already_exists(msg.clone()),
            ErrorKind::FailedPrecondition => Status::failed_precondition(msg.clone()),
            ErrorKind::Unimplemented => Status::unimplemented(msg.clone()),
            ErrorKind::ResourceExhausted => Status::resource_exhausted(msg.clone()),
            ErrorKind::Internal => Status::internal(msg.clone()),
        }
    }
}

#[tonic::async_trait]
impl AndlerService for DaemonService {
    type StreamInstanceLogsStream =
        Pin<Box<dyn Stream<Item = Result<LogLineResponse, Status>> + Send + 'static>>;

    type StreamResourceMetricsStream =
        Pin<Box<dyn Stream<Item = Result<ResourceMetricsResponse, Status>> + Send + 'static>>;

    type StreamDaemonLogsStream =
        Pin<Box<dyn Stream<Item = Result<DaemonLogLine, Status>> + Send + 'static>>;

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
            .create_linux_instance(
                cfg,
                andler_core::paths::instances_root(),
                ovmf_vars_template,
            )
            .await?;
        Ok(Response::new(CreateInstanceResponse {
            instance_id: id.to_string(),
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
            instance_id: id.to_string(),
        }))
    }

    async fn start_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
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
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        self.daemon.pause_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn resume_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        self.daemon.resume_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_instance_status(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<InstanceStatusResponse>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
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
                let (state, error_message) = convert::instance_state_to_proto(&summary.state);
                InstanceListEntry {
                    instance_id: summary.id.to_string(),
                    name: summary.name,
                    state: state as i32,
                    error_message,
                    broken_reason: summary.broken_reason,
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
        let id = self
            .daemon
            .resolve_instance_id(&request.instance_id)
            .await?;
        self.daemon.remove_instance_entry(id, request.purge).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_instance_config(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<GetInstanceConfigResponse>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        let config = self.daemon.get_instance_config(id).await?;
        Ok(Response::new(config.into()))
    }

    async fn get_config_status(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<GetConfigStatusResponse>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        let (config, diffs, file_error, live_resolution) = self.daemon.config_status(id).await?;
        let (state, _) = convert::instance_state_to_proto(&self.daemon.status(id).await?.state);

        let mut config_diffs = Vec::with_capacity(diffs.len());
        for (key, file_value, memory_value) in diffs {
            config_diffs.push(ConfigKeyDiff {
                key,
                file_value,
                memory_value,
            });
        }

        Ok(Response::new(GetConfigStatusResponse {
            instance_id: id.to_string(),
            name: config.name,
            state: state as i32,
            diffs: config_diffs,
            live_resolution: live_resolution.map(|r| format!("{}x{}", r.width, r.height)),
            file_error,
        }))
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

    // tonic::Status in the stream error slot is required by the gRPC API — boxing it
    // would add indirection for no gain, so silence the size lint here.
    #[allow(clippy::result_large_err)]
    async fn stream_instance_logs(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamInstanceLogsStream>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        let inner = self.daemon.stream_instance_logs(id).await?;
        let mapped = inner.map(|line| Ok(LogLineResponse::from(line)));
        Ok(Response::new(Box::pin(mapped)))
    }

    // tonic::Status in the stream error slot is required by the gRPC API — boxing it
    // would add indirection for no gain, so silence the size lint here.
    #[allow(clippy::result_large_err)]
    async fn stream_resource_metrics(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamResourceMetricsStream>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;
        let inner = self.daemon.stream_resource_metrics(id).await?;
        let mapped = inner.map(|m| Ok(ResourceMetricsResponse::from(m)));
        Ok(Response::new(Box::pin(mapped)))
    }

    async fn clone_instance(
        &self,
        request: Request<CloneInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let req = request.into_inner();
        let source_id = self
            .daemon
            .resolve_instance_id(&req.source_instance_id)
            .await?;
        let mode = CloneMode::try_from(req.mode())?;

        let id = self
            .daemon
            .clone_instance(source_id, req.new_name, req.instances_root.into(), mode)
            .await?;

        Ok(Response::new(CreateInstanceResponse {
            instance_id: id.to_string(),
        }))
    }

    async fn export_instance_disk(
        &self,
        request: Request<ExportInstanceDiskRequest>,
    ) -> Result<Response<ExportInstanceDiskResponse>, Status> {
        let req = request.into_inner();
        let source_id = self
            .daemon
            .resolve_instance_id(&req.source_instance_id)
            .await?;

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

        self.daemon
            .restore_snapshot(id, req.tag, req.branch)
            .await?;

        Ok(Response::new(Empty {}))
    }

    async fn delete_snapshot(
        &self,
        request: Request<DeleteSnapshotRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon
            .delete_snapshot(id, req.tag, req.timeout_secs)
            .await?;

        Ok(Response::new(Empty {}))
    }

    async fn list_snapshots(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<ListSnapshotsResponse>, Status> {
        let id = self
            .daemon
            .resolve_instance_id(&request.into_inner().instance_id)
            .await?;

        let records = self.daemon.list_snapshots(id).await?;

        let snapshots = records
            .into_iter()
            .map(|r| SnapshotEntry {
                snapshot_id: r.id.to_string(),
                tag: r.tag,
                description: r.description.unwrap_or_default(),
                created_at: r.created_at,
                branch: r.branch.unwrap_or_default(),
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

        self.daemon
            .install_guest_agent(id, req.package, req.offline)
            .await?;

        Ok(Response::new(Empty {}))
    }

    async fn remove_guest_agent(
        &self,
        request: Request<RemoveGuestAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;

        self.daemon
            .remove_guest_agent(id, req.package, req.offline)
            .await?;

        Ok(Response::new(Empty {}))
    }

    async fn guest_provision(
        &self,
        request: Request<GuestProvisionRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        let ops = andler_rpc::provision_convert::provision_ops_from_proto(&req.ops)?;

        self.daemon.guest_provision(id, ops).await?;

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

        Ok(Response::new(ListGuestPackagesResponse {
            packages: entries,
        }))
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

    async fn attach_disk(
        &self,
        request: Request<AttachDiskRequest>,
    ) -> Result<Response<AttachDiskResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        let path = if req.path.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(req.path))
        };
        let (path, index) = self.daemon.attach_disk(id, path, req.size_bytes).await?;
        Ok(Response::new(AttachDiskResponse {
            path: path.to_string_lossy().into_owned(),
            index: index as u32,
        }))
    }

    async fn detach_disk(
        &self,
        request: Request<DetachDiskRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        self.daemon
            .detach_disk(id, std::path::PathBuf::from(req.path))
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn attach_network(
        &self,
        request: Request<AttachNetworkRequest>,
    ) -> Result<Response<AttachNetworkResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        let network = req
            .network
            .ok_or_else(|| Status::invalid_argument("network is required"))?
            .try_into()?;
        let index = self.daemon.attach_network(id, network).await?;
        Ok(Response::new(AttachNetworkResponse {
            index: index as u32,
        }))
    }

    async fn detach_network(
        &self,
        request: Request<DetachNetworkRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        self.daemon.detach_network(id, req.index as usize).await?;
        Ok(Response::new(Empty {}))
    }

    async fn list_operations(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<OpListResponse>, Status> {
        let operations = self
            .daemon
            .list_operations()
            .await
            .into_iter()
            .map(|op| OperationInfo {
                op_id: op.op_id,
                instance_id: op.instance_id.to_string(),
                kind: format!("{:?}", op.kind),
                phases: op
                    .phases
                    .into_iter()
                    .map(|(name, weight)| OperationPhase {
                        name,
                        weight: weight as f64,
                    })
                    .collect(),
                progress: op.progress as f64,
                state: format!("{:?}", op.state),
                error: op.error.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(OpListResponse { operations }))
    }

    async fn cancel_operation(
        &self,
        request: Request<OpCancelRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        self.daemon.cancel_operation(&req.op_id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_version(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<VersionResponse>, Status> {
        Ok(Response::new(VersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    type StreamEventsStream =
        Pin<Box<dyn Stream<Item = Result<DaemonEventMessage, Status>> + Send>>;

    async fn stream_daemon_logs(
        &self,
        request: Request<DaemonLogsRequest>,
    ) -> Result<Response<Self::StreamDaemonLogsStream>, Status> {
        let req = request.into_inner();
        let since_ms = req.since_ms.unwrap_or(0);

        let ring = self.log_ring.clone();
        let snapshot = ring.snapshot(since_ms);
        let rx = if req.follow {
            Some(ring.subscribe())
        } else {
            None
        };

        let stream = async_stream::stream! {
            for (ts_ms, line) in snapshot {
                yield Ok(DaemonLogLine { ts_ms, line });
            }
            if let Some(mut rx) = rx {
                loop {
                    match rx.recv().await {
                        Ok(line) => yield Ok(DaemonLogLine {
                            ts_ms: chrono::Utc::now().timestamp_millis() as u64,
                            line,
                        }),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn get_daemon_metrics(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<DaemonMetricsResponse>, Status> {
        let (instance_count, running_count, active_ops, running_ram_bytes) = {
            let supervisors = self.daemon.supervisors.read().await;
            let mut active = 0usize;
            for handle in supervisors.values() {
                if handle.active_operation().await?.is_some() {
                    active += 1;
                }
            }
            let running = supervisors
                .values()
                .filter(|h| h.state() == andler_core::InstanceState::Running)
                .count();
            let running_ram_bytes: u64 = supervisors
                .values()
                .filter(|h| h.state() == andler_core::InstanceState::Running)
                .map(|h| h.config().memory.size_bytes)
                .sum();
            (supervisors.len(), running, active, running_ram_bytes)
        };
        let qmp_reconnects: u64 = self
            .daemon
            .backends
            .values()
            .map(|b| b.qmp_reconnect_count())
            .sum();
        let snap =
            self.metrics
                .snapshot(instance_count, running_count, active_ops, running_ram_bytes);

        Ok(Response::new(DaemonMetricsResponse {
            latency: snap
                .latency
                .into_iter()
                .map(|m| MethodLatency {
                    method: m.method,
                    count: m.count,
                    p50_ms: m.p50_ms,
                    p99_ms: m.p99_ms,
                })
                .collect(),
            by_code: snap
                .by_code
                .into_iter()
                .map(|(code, count)| CodeCount { code, count })
                .collect(),
            instance_count: snap.instance_count as u64,
            running_count: snap.running_count as u64,
            active_ops: snap.active_ops as u64,
            qmp_reconnects,
            running_ram_bytes,
        }))
    }

    async fn stream_events(
        &self,
        request: Request<EventStreamRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let req = request.into_inner();
        let filter = if req.instance_id.is_empty() {
            None
        } else {
            Some(self.daemon.resolve_instance_id(&req.instance_id).await?)
        };
        let inner = self.daemon.stream_events(filter);
        // tonic::Status in the stream error slot is required by the gRPC API — boxing it
        // would add indirection for no gain, so silence the size lint here.
        #[allow(clippy::result_large_err)]
        let mapped = inner.map(|event| {
            let kind = match &event.kind {
                EventKind::Lifecycle { .. } => "Lifecycle",
                EventKind::Operation { .. } => "Operation",
                EventKind::Qmp { .. } => "Qmp",
                EventKind::Readiness { .. } => "Readiness",
                EventKind::Log { .. } => "Log",
            };
            Ok(DaemonEventMessage {
                ts_ms: event.ts_ms,
                instance_id: event
                    .instance_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                kind: kind.to_string(),
                detail: serde_json::to_string(&event.kind).unwrap_or_default(),
            })
        });
        Ok(Response::new(Box::pin(mapped)))
    }

    async fn exec_command(
        &self,
        request: Request<ExecCommandRequest>,
    ) -> Result<Response<ExecCommandResponse>, Status> {
        let req = request.into_inner();
        let id = self.daemon.resolve_instance_id(&req.instance_id).await?;
        let output = self
            .daemon
            .guest_exec_command(id, req.argv, req.timeout_secs)
            .await?;
        Ok(Response::new(ExecCommandResponse {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        }))
    }
}
