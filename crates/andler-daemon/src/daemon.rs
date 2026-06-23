//! `Daemon` — связывает реестр `HypervisorBackend`-реализаций
//! (`andler-qemu::QemuBackend`, в перспективе `andler-vmm`) с FSM из
//! `andler-core` и временным in-memory хранением `InstanceConfig`.
//!
//! Это ядро `andlerd` без сети и без персистентности: `andler-rpc`/gRPC и
//! `andler-store`/SQLite — следующие слои поверх этой структуры, не часть
//! неё. `Daemon` отвечает на вопрос "что происходит, когда нужно создать,
//! запустить, поставить на паузу или остановить инстанс", а не "как до
//! этого добраться по сети" или "как это переживёт перезапуск процесса".
//!
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §5 (API/RPC) — методы
//! здесь соответствуют будущим gRPC-методам по смыслу один-к-одному, но
//! сами пока не знают про gRPC.

use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendHandle, BackendKind, BackendStatus, HypervisorBackend, InstanceConfig,
    InstanceEvent, InstanceId, InstanceState,
};
use andler_qemu::QemuBackend;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum DaemonError {
    /// Инстанс с таким `InstanceId` не зарегистрирован в этом `Daemon`
    /// (не путать с `BackendError::HandleNotFound` — это про конкретный
    /// backend-хэндл; эта ошибка — про то, что демон вообще не знает про
    /// такой `InstanceId`).
    #[error("instance {0:?} not found")]
    InstanceNotFound(InstanceId),

    /// Запрошен `BackendKind`, для которого в реестре нет реализации
    /// (на данный момент в реестре по умолчанию есть только `Qemu` —
    /// см. `Daemon::new`).
    #[error("no backend registered for {0:?}")]
    NoBackendRegistered(BackendKind),

    /// Переход FSM не разрешён из текущего состояния (см.
    /// `andler_core::fsm`) — например, `pause` для инстанса, который
    /// ещё не был запущен.
    #[error("invalid state transition: {0}")]
    InvalidTransition(#[from] andler_core::FsmError),

    /// Backend вернул ошибку при выполнении операции.
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
}

/// Внутреннее состояние одного инстанса с точки зрения `Daemon`:
/// конфигурация (нужна, чтобы повторно резолвить `spawn`, и для будущего
/// сохранения в `andler-store`), текущее состояние FSM, и backend-хэндл —
/// есть только после первого успешного `start_instance` (до этого инстанс
/// существует как конфигурация, но ничего не запущено).
struct InstanceRecord {
    config: InstanceConfig,
    state: InstanceState,
    handle: Option<BackendHandle>,
}

/// Ядро `andlerd`: реестр backend'ов + текущее in-memory состояние
/// инстансов.
///
/// `instances` — под `tokio::sync::RwLock`, не `Mutex`: `status()`
/// (предполагаемо частый вызов, например при опросе из GUI) — это только
/// чтение записи демона о состоянии плюс один вызов backend'а; не имеет
/// смысла блокировать другие конкурентные чтения статусов на время одного
/// такого вызова так же, как блокировались бы записи.
pub struct Daemon {
    backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    instances: RwLock<HashMap<InstanceId, InstanceRecord>>,
}

impl Daemon {
    /// Создаёт `Daemon` с реестром backend'ов по умолчанию: `Qemu` ->
    /// `QemuBackend`. `Vmm` сознательно не регистрируется здесь —
    /// `andler-vmm` пустой каркас (см. его README), регистрация
    /// несуществующей полноценной реализации не добавила бы ценности;
    /// когда `andler-vmm` будет готов к регистрации, эта точка —
    /// естественное место, где это сделать.
    pub fn new() -> Self {
        let mut backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>> = HashMap::new();
        backends.insert(BackendKind::Qemu, Arc::new(QemuBackend::new()));

        Daemon {
            backends,
            instances: RwLock::new(HashMap::new()),
        }
    }

    fn backend_for(&self, kind: BackendKind) -> Result<&Arc<dyn HypervisorBackend>, DaemonError> {
        self.backends
            .get(&kind)
            .ok_or(DaemonError::NoBackendRegistered(kind))
    }

    /// Регистрирует новый инстанс с состоянием `Created`. Ничего не
    /// запускает — соответствует `andler_core::fsm::InstanceState::Created`:
    /// конфигурация принята и сохранена (пока только in-memory), процесс
    /// ещё не существует. Backend для `cfg.backend` должен быть
    /// зарегистрирован — проверяется здесь же, до сохранения записи, чтобы
    /// не создавать инстанс, который заведомо невозможно запустить.
    pub async fn create_instance(&self, cfg: InstanceConfig) -> Result<InstanceId, DaemonError> {
        self.backend_for(cfg.backend)?;

        let id = cfg.id;
        let mut instances = self.instances.write().await;
        instances.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Created,
                handle: None,
            },
        );

        Ok(id)
    }

    /// Запускает ранее созданный инстанс: `Created -> Starting -> Running`
    /// (см. `andler_core::fsm`). При ошибке backend'а переводит запись в
    /// `Error { message }`, а не оставляет её в промежуточном `Starting`
    /// — `Starting` без последующего `StartCompleted`/`Fail` означало бы
    /// зависшую запись, на которую не может среагировать ни одна другая
    /// операция (FSM не разрешает из `Starting` ничего, кроме `StartCompleted`
    /// и `Fail`, см. `fsm.rs`).
    pub async fn start_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        record.state = record.state.clone().apply(InstanceEvent::Start)?;

        let backend = self.backend_for(record.config.backend)?;
        match backend.spawn(&record.config).await {
            Ok(handle) => {
                record.handle = Some(handle);
                record.state = record.state.clone().apply(InstanceEvent::StartCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        }
    }

    /// Останавливает инстанс. `graceful` передаётся напрямую в
    /// `HypervisorBackend::stop` (см. его документацию про текущее
    /// значение "graceful" без полноценного ACPI-сигнала, пока в
    /// `andler-qemu` нет снапшота/более развитого qmp.rs).
    pub async fn stop_instance(&self, id: InstanceId, graceful: bool) -> Result<(), DaemonError> {
        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;

        record.state = record.state.clone().apply(InstanceEvent::Stop)?;

        let backend = self.backend_for(record.config.backend)?;
        match backend.stop(&handle, graceful).await {
            Ok(()) => {
                record.handle = None;
                record.state = record.state.clone().apply(InstanceEvent::StopCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        }
    }

    /// Приостанавливает работающий инстанс. В отличие от
    /// `start_instance`/`stop_instance`, не меняет `InstanceState` записи
    /// демона напрямую при успехе — синхронизация с реальным состоянием
    /// гостя (`Running`/`Paused`) происходит через `status()`, который
    /// спрашивает backend (а тот, в свою очередь, QMP `query-status`) о
    /// реальном состоянии, а не предполагает его исходя из того, что
    /// команда была отправлена успешно. Это сознательное решение: FSM
    /// `Daemon`-записи отражает то, что демон попросил сделать, а не то,
    /// что гость гарантированно уже сделал — при ошибке после успешной
    /// отправки команды (теоретическая гонка) `status()` всё равно покажет
    /// правду.
    pub async fn pause_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;
        let backend = self.backend_for(record.config.backend)?;

        backend.pause(&handle).await.map_err(DaemonError::Backend)
    }

    /// Возобновляет приостановленный инстанс. См. документацию
    /// `pause_instance` про то, почему `InstanceState` записи демона не
    /// меняется здесь напрямую.
    pub async fn resume_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;
        let backend = self.backend_for(record.config.backend)?;

        backend.resume(&handle).await.map_err(DaemonError::Backend)
    }

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
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceKind, MemoryConfig, NetworkConfig, RenderBackend,
    };
    use std::path::PathBuf;

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[tokio::test]
    async fn create_instance_registers_with_created_state() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;

        let returned_id = daemon.create_instance(cfg).await.unwrap();
        assert_eq!(returned_id, id);

        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Created);
    }

    #[tokio::test]
    async fn status_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.status(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn start_instance_rejects_passthrough_via_backend_validation() {
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.start_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::InvalidConfig { .. })
        ));

        // Запись демона должна перейти в Error, не зависнуть в Starting.
        let status = daemon.status(id).await.unwrap();
        assert!(matches!(status.state, InstanceState::Error { .. }));
    }

    #[tokio::test]
    async fn pause_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.pause_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn pause_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.pause_instance(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn resume_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.resume_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn resume_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.resume_instance(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn stop_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.stop_instance(id, true).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn double_create_with_same_id_overwrites_record() {
        // Документирует текущее поведение: create_instance не проверяет
        // существование записи и просто перезаписывает её — это осознанно
        // не валидируется здесь, так как InstanceId генерируется через
        // InstanceId::new() (UUID v4) и коллизия при нормальном
        // использовании практически невозможна; явная защита от повторного
        // create с тем же ID добавится, если/когда появится API, которое
        // действительно может передать чужой ID (например, восстановление
        // из andler-store).
        let daemon = Daemon::new();
        let cfg1 = sample_config();
        let id = cfg1.id;
        daemon.create_instance(cfg1).await.unwrap();

        let mut cfg2 = sample_config();
        cfg2.id = id;
        cfg2.name = "renamed".to_string();
        daemon.create_instance(cfg2).await.unwrap();

        let instances = daemon.instances.read().await;
        assert_eq!(instances.get(&id).unwrap().config.name, "renamed");
    }
}
