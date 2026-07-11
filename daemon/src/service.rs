//! Реализация сгенерированного gRPC-трейта
//! (`andler_rpc::proto::andler_service_server::AndlerService`) как тонкой
//! обёртки над `Daemon` — один метод трейта на один метод `Daemon`, без
//! бизнес-логики здесь (она целиком в `daemon.rs`). Соответствует тому, как
//! README `andler-daemon` описывал будущий `service.rs` с самого начала.

use std::pin::Pin;
use std::sync::Arc;

use andler_core::{CloneMode, InstanceConfig};
use andler_rpc::convert;
use andler_rpc::proto::andler_service_server::AndlerService;
use andler_rpc::proto::{
    CloneInstanceRequest, CreateAndroidInstanceRequest, CreateInstanceRequest,
    CreateInstanceResponse, CreateSnapshotRequest, CreateSnapshotResponse, DeleteSnapshotRequest,
    Empty, ExportInstanceDiskRequest, ExportInstanceDiskResponse, GetInstanceConfigResponse,
    GuestPackageEntry, InstallGuestAgentRequest, InstanceIdRequest, InstanceListEntry,
    InstanceStatusResponse, ListGuestPackagesResponse, ListInstancesResponse, ListSnapshotsResponse,
    LogLineResponse, RemoveGuestAgentRequest, RemoveInstanceRequest, ResourceMetricsResponse,
    RestoreSnapshotRequest, SnapshotEntry, StopInstanceRequest, UpdateInstanceConfigRequest,
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

/// Отображение `DaemonError` на gRPC-статусы. `InstanceNotFound` ->
/// `NOT_FOUND`, отсутствие реализации backend'а (`NoBackendRegistered`,
/// `BackendError::NotImplemented`) -> `UNIMPLEMENTED` (соответствует
/// "Контракту для незавершённых backend'ов" из §4 архитектурного плана —
/// демон не должен падать, он должен вернуть штатный gRPC-статус),
/// нарушение FSM/отсутствие хэндла/`InstanceNotRemovable`/
/// `InstanceNotClonable`/`SharedBaseNotSupportedForLinuxVm`/
/// `InstanceHasLiveClones` -> `FAILED_PRECONDITION` (клиент мог бы
/// исправить ситуацию, изменив порядок вызовов — например, `stop` перед
/// `remove`/`clone`/`export`, или удалить клоны перед `remove --purge`
/// источника), остальное -> `INTERNAL`.
///
/// `DaemonError::Restore` — отдельная ветка, не часть `_ => INTERNAL`:
/// она недостижима из любого метода `AndlerService` (всю обработку
/// запросов выполняют `create_instance`/`start_instance`/`stop_instance`/
/// `pause_instance`/`resume_instance`/`status`/`create_android_instance`/
/// `list_instances`/`remove_instance`, ни один из которых не вызывает
/// `Daemon::restore` — `restore` вызывается только однократно в
/// `main.rs`, до того, как сервис вообще создан). `Status::internal`
/// здесь — это документирующий, а не ожидаемый путь: если он когда-либо
/// сработает на практике, это означает, что `DaemonService` стал
/// вызывать `restore` из обработчика запроса, что само по себе было бы
/// ошибкой архитектуры, а не штатной ситуацией, которую стоит
/// транслировать в более специфичный статус.
impl From<DaemonError> for Status {
    fn from(err: DaemonError) -> Self {
        match &err {
            DaemonError::InstanceNotFound(_) => Status::not_found(err.to_string()),
            DaemonError::NoBackendRegistered(_) => Status::unimplemented(err.to_string()),
            DaemonError::InvalidTransition(_) => Status::failed_precondition(err.to_string()),
            DaemonError::InstanceNotRemovable(_, _) => {
                Status::failed_precondition(err.to_string())
            }
            // Та же категория, что `InstanceNotRemovable` выше — клиент
            // мог бы исправить ситуацию, изменив порядок вызовов (stop
            // перед clone/export; убрать клоны перед purge), не
            // INTERNAL.
            DaemonError::InstanceNotClonable(_, _) => Status::failed_precondition(err.to_string()),
            DaemonError::SharedBaseNotSupportedForLinuxVm(_) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::InstanceHasLiveClones(_, _) => {
                Status::failed_precondition(err.to_string())
            }
            // Snapshot-ошибки: клиент может исправить, изменив параметры
            // или порядок вызовов.
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
            // The VM process is gone — same client-facing category as
            // "instance not found in the way you expected it to be":
            // fixable by the client re-checking status, not a server bug.
            DaemonError::Backend(andler_core::BackendError::ProcessNotRunning) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::Backend(_)
            | DaemonError::Disk(_)
            | DaemonError::Io { .. }
            | DaemonError::Restore(_) => Status::internal(err.to_string()),
            // OVMF VARS provisioning failed for a new Android instance
            // (see `instance_ops.rs`) — not a client-input problem, akin
            // to the other Backend/Disk/Io/Restore internal errors above.
            DaemonError::Firmware(_) => Status::internal(err.to_string()),
            // Same category as `ConvertError -> Status::invalid_argument`
            // in `andler-rpc` (see comment below): these three are all
            // about the client's `instance_id` string itself being
            // malformed/unresolvable/ambiguous, not about server state.
            DaemonError::EmptyInstanceRef => Status::invalid_argument(err.to_string()),
            DaemonError::MalformedInstanceRef(_) => Status::invalid_argument(err.to_string()),
            DaemonError::InstanceRefNotFound(_) => Status::not_found(err.to_string()),
            DaemonError::AmbiguousInstanceId { .. } => Status::invalid_argument(err.to_string()),
            // `andler edit` sent back a config that would desync the
            // stored config from what's actually on disk/in the daemon —
            // same category as the instance-ref errors above: the client
            // (or the human editing the TOML) can fix these by not
            // touching those fields, not a server-state problem.
            DaemonError::ConfigIdMismatch { .. } => Status::invalid_argument(err.to_string()),
            DaemonError::ConfigKindChanged(_) => Status::invalid_argument(err.to_string()),
            DaemonError::ConfigDiskPathChanged(_) => Status::invalid_argument(err.to_string()),
            // Guest agent operations: client can fix by stopping the VM
            // or ensuring guest agent is installed.
            DaemonError::GuestAgentUnavailable { .. } => Status::failed_precondition(err.to_string()),
        }
    }
}

// `impl From<convert::ConvertError> for Status` живёт в `andler-rpc`
// (`src/convert.rs`), не здесь — `ConvertError` и `Status` оба чужие для
// `andler-daemon` (orphan rule запрещает `impl ForeignTrait for ForeignType`
// в третьем крейте), а `andler-rpc` уже зависит от `tonic` и владеет
// `ConvertError`, так что там это разрешено.

#[tonic::async_trait]
impl AndlerService for DaemonService {
    /// Тип возвращаемого потока для `stream_instance_logs` — требуется
    /// сгенерированным трейтом для server-streaming RPC (см.
    /// `rpc StreamInstanceLogs` в `andler.proto`). `Pin<Box<dyn Stream<...>
    /// + Send>>` — стандартная forма для такого ассоциированного типа в
    /// `tonic`, не специфичная для этого метода деталь; конкретная
    /// реализация (`Daemon::stream_instance_logs`, смэпленная через
    /// `.map(...)` ниже) уже `'static` и `Send` сама по себе (см.
    /// документацию `Daemon::stream_instance_logs` за тем, почему).
    type StreamInstanceLogsStream =
        Pin<Box<dyn Stream<Item = Result<LogLineResponse, Status>> + Send + 'static>>;

    type StreamResourceMetricsStream =
        Pin<Box<dyn Stream<Item = Result<ResourceMetricsResponse, Status>> + Send + 'static>>;

    /// Создаёт `LinuxVm`-инстанс из явного `InstanceConfig`, переданного
    /// клиентом целиком. В отличие от `create_android_instance`, здесь нет
    /// промежуточного резолва профиля/создания overlay-диска — конвертация
    /// `CreateInstanceRequest -> InstanceConfig` (`andler_rpc::convert`)
    /// уже даёт полный, готовый к `Daemon::create_instance` конфиг. См.
    /// комментарий у `rpc CreateInstance` в `andler.proto` про то, почему
    /// этот путь ограничен на `LinuxVm` и не принимает `AndroidVm`.
    async fn create_instance(
        &self,
        request: Request<CreateInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let mut cfg = InstanceConfig::try_from(request.into_inner())?;

        if cfg.firmware.ovmf_code_path.as_os_str().is_empty() {
            cfg.firmware.ovmf_code_path = self.ovmf.code.clone();
        }

        // `cfg.firmware.ovmf_vars_path` at this point holds whatever the
        // client sent as `--ovmf-vars-template` (possibly empty). If the
        // client gave one explicitly, it must win over the daemon's
        // auto-detected template — this used to be unconditionally
        // overwritten by `self.ovmf.vars_template` regardless of what the
        // client asked for (a real bug: an explicit `--ovmf-vars-template`
        // was silently discarded every time). `create_android_instance`
        // right below already gets this right — this brings the Linux
        // path in line with it.
        let ovmf_vars_template = if cfg.firmware.ovmf_vars_path.as_os_str().is_empty() {
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

        // Если клиент не передал ovmf_vars_template — используем
        // авто-определённый шаблон. Аналогично ovmf_code_path выше.
        let ovmf_vars_template = if req.ovmf_vars_template.is_empty() {
            self.ovmf.vars_template.clone()
        } else {
            req.ovmf_vars_template.into()
        };

        let id = self
            .daemon
            .create_android_instance(
                profile,
                req.name,
                req.base_image_path.into(),
                req.instances_root.into(),
                req.overlay_size_bytes,
                ovmf_vars_template,
                if req.magisk_dir.is_empty() {
                    None
                } else {
                    Some(req.magisk_dir.into())
                },
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

    /// Соответствует `Daemon::list_instances`. Конвертация
    /// `daemon::InstanceSummary -> proto::InstanceListEntry` живёт здесь,
    /// а не в `andler_rpc::convert` (как остальные конвертации в этом
    /// файле) — `InstanceSummary` определён в `andler-daemon::daemon`, а
    /// `andler-rpc` не зависит от `andler-daemon` (зависимость обратная);
    /// `convert.rs` физически не может на него сослаться.
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

    /// Соответствует `Daemon::get_instance_config`. Конвертация
    /// `InstanceConfig -> GetInstanceConfigResponse` целиком живёт в
    /// `andler_rpc::convert` (`impl From<InstanceConfig> for
    /// proto::GetInstanceConfigResponse`) — в отличие от
    /// `list_instances` выше, тут нет зависимости от типа, специфичного
    /// для `andler-daemon`: `InstanceConfig` — тип `andler-core`, на
    /// который `andler-rpc` и так ссылается.
    async fn get_instance_config(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<GetInstanceConfigResponse>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let config = self.daemon.get_instance_config(id).await?;
        Ok(Response::new(config.into()))
    }

    /// Соответствует `Daemon::update_instance_config` (`andler edit`).
    /// `instance_ref` разрешается тем же путём, что и `instance_id` у
    /// `GetInstanceConfig` (партиал-ID через `resolve_instance_id`), а не
    /// напрямую как `InstanceId` — конвертация запроса в `InstanceConfig`
    /// нуждается в уже разрешённом `id` (см. doc-комментарий
    /// `convert::update_request_to_instance_config`), поэтому резолв идёт
    /// первым шагом, до конвертации.
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

    /// Соответствует `Daemon::stream_instance_logs`. Не возвращает gRPC
    /// ошибку для инстанса без запущенного backend'а — `Daemon` уже сам
    /// отдаёт пустой поток в этом случае (см. документацию там), здесь
    /// просто транслируется `LogLine -> LogLineResponse` (см.
    /// `andler_rpc::convert`) по каждому элементу.
    async fn stream_instance_logs(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamInstanceLogsStream>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let inner = self.daemon.stream_instance_logs(id).await?;
        let mapped = inner.map(|line| Ok(LogLineResponse::from(line)));
        Ok(Response::new(Box::pin(mapped)))
    }

    /// Соответствует `Daemon::stream_resource_metrics`. Server-streaming —
    /// идентичен `stream_instance_logs` по паттерну: конвертирует
    /// `ResourceMetrics -> ResourceMetricsResponse` по каждому элементу.
    async fn stream_resource_metrics(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Self::StreamResourceMetricsStream>, Status> {
        let id = self.daemon.resolve_instance_id(&request.into_inner().instance_id).await?;
        let inner = self.daemon.stream_resource_metrics(id).await?;
        let mapped = inner.map(|m| Ok(ResourceMetricsResponse::from(m)));
        Ok(Response::new(Box::pin(mapped)))
    }

    /// Соответствует `Daemon::clone_instance`. `mode` — proto-enum
    /// (`i32` на уровне сообщения, см. `req.mode()`), конвертация в
    /// доменный `CloneMode` через `TryFrom` (`andler_rpc::convert`) —
    /// `CLONE_MODE_UNSPECIFIED` отклоняется тем же путём, что и
    /// `ANDROID_VERSION_UNSPECIFIED`/`ROOT_MODE_UNSPECIFIED` у
    /// `create_android_instance`.
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

    /// Соответствует `Daemon::export_instance_disk`. Не создаёт новый
    /// инстанс — ответ эхо подтверждает `dest_path`, не возвращает
    /// `instance_id` (см. документацию `rpc ExportInstanceDisk` за тем,
    /// почему это отдельный метод, не вариант `CloneInstance`).
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

    // --- Snapshot handlers ------------------------------------------------

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
}
