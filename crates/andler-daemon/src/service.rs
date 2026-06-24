//! Реализация сгенерированного gRPC-трейта
//! (`andler_rpc::proto::andler_service_server::AndlerService`) как тонкой
//! обёртки над `Daemon` — один метод трейта на один метод `Daemon`, без
//! бизнес-логики здесь (она целиком в `daemon.rs`). Соответствует тому, как
//! README `andler-daemon` описывал будущий `service.rs` с самого начала.

use std::sync::Arc;

use andler_rpc::convert;
use andler_rpc::proto::andler_service_server::AndlerService;
use andler_rpc::proto::{
    CreateAndroidInstanceRequest, CreateInstanceResponse, Empty, InstanceIdRequest,
    InstanceStatusResponse, StopInstanceRequest,
};
use tonic::{Request, Response, Status};

use crate::daemon::{Daemon, DaemonError};

pub struct DaemonService {
    daemon: Arc<Daemon>,
}

impl DaemonService {
    pub fn new(daemon: Arc<Daemon>) -> Self {
        DaemonService { daemon }
    }
}

/// Отображение `DaemonError` на gRPC-статусы. `InstanceNotFound` ->
/// `NOT_FOUND`, отсутствие реализации backend'а (`NoBackendRegistered`,
/// `BackendError::NotImplemented`) -> `UNIMPLEMENTED` (соответствует
/// "Контракту для незавершённых backend'ов" из §4 архитектурного плана —
/// демон не должен падать, он должен вернуть штатный gRPC-статус),
/// нарушение FSM/отсутствие хэндла -> `FAILED_PRECONDITION` (клиент мог бы
/// исправить ситуацию, изменив порядок вызовов), остальное -> `INTERNAL`.
impl From<DaemonError> for Status {
    fn from(err: DaemonError) -> Self {
        match &err {
            DaemonError::InstanceNotFound(_) => Status::not_found(err.to_string()),
            DaemonError::NoBackendRegistered(_) => Status::unimplemented(err.to_string()),
            DaemonError::InvalidTransition(_) => Status::failed_precondition(err.to_string()),
            DaemonError::Backend(andler_core::BackendError::NotImplemented { .. }) => {
                Status::unimplemented(err.to_string())
            }
            DaemonError::Backend(andler_core::BackendError::HandleNotFound(_)) => {
                Status::failed_precondition(err.to_string())
            }
            DaemonError::Backend(_) | DaemonError::Disk(_) | DaemonError::Io { .. } => {
                Status::internal(err.to_string())
            }
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
    async fn create_android_instance(
        &self,
        request: Request<CreateAndroidInstanceRequest>,
    ) -> Result<Response<CreateInstanceResponse>, Status> {
        let req = request.into_inner();
        let profile_msg = req
            .profile
            .ok_or_else(|| Status::invalid_argument("missing profile"))?;
        let profile = andler_core::AndroidProfile::try_from(profile_msg)?;

        let id = self
            .daemon
            .create_android_instance(
                profile,
                req.name,
                req.base_image_path.into(),
                req.instances_root.into(),
                req.overlay_size_bytes,
                req.ovmf_vars_template.into(),
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
        let id = convert::parse_instance_id(&request.into_inner().instance_id)?;
        self.daemon.start_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn stop_instance(
        &self,
        request: Request<StopInstanceRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let id = convert::parse_instance_id(&req.instance_id)?;
        self.daemon.stop_instance(id, req.graceful).await?;
        Ok(Response::new(Empty {}))
    }

    async fn pause_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = convert::parse_instance_id(&request.into_inner().instance_id)?;
        self.daemon.pause_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn resume_instance(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = convert::parse_instance_id(&request.into_inner().instance_id)?;
        self.daemon.resume_instance(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_instance_status(
        &self,
        request: Request<InstanceIdRequest>,
    ) -> Result<Response<InstanceStatusResponse>, Status> {
        let id = convert::parse_instance_id(&request.into_inner().instance_id)?;
        let status = self.daemon.status(id).await?;
        let (state, error_message) = convert::instance_state_to_proto(&status.state);
        Ok(Response::new(InstanceStatusResponse {
            state: state as i32,
            error_message,
            detail: status.detail.unwrap_or_default(),
        }))
    }
}
