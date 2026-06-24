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
use std::path::PathBuf;
use std::sync::Arc;

use andler_core::{
    AndroidProfile, BackendError, BackendHandle, BackendKind, BackendStatus, HypervisorBackend,
    InstanceConfig, InstanceEvent, InstanceId, InstanceState,
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

    /// Ошибка `andler-disk` при создании overlay-диска для Android-инстанса
    /// (см. `create_android_instance`).
    #[error("disk error: {0}")]
    Disk(#[from] andler_disk::DiskError),

    /// Ошибка файловой системы вне `andler-disk` — на данный момент это
    /// только подготовка персональной копии `OVMF_VARS` для нового
    /// Android-инстанса (создание каталога инстанса, копирование шаблона).
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
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

    /// Резолвит `AndroidProfile` в полноценный инстанс и регистрирует его —
    /// связывает `AndroidProfile::resolve()` (домен, `andler-core`) с
    /// реальным созданием overlay-диска на диске (`andler-disk::overlay`)
    /// и персональной копии `OVMF_VARS`, и только потом передаёт получившийся
    /// `InstanceConfig` в `create_instance` (то есть проходит ту же
    /// валидацию backend'а и попадает в тот же реестр, что и инстанс,
    /// созданный напрямую из готового `InstanceConfig`).
    ///
    /// `instances_root` — каталог, под которым у каждого инстанса свой
    /// подкаталог `<instances_root>/<InstanceId>/` (overlay-диск и
    /// `VARS.fd` внутри него) — соответствует `/var/lib/andler/instances/`
    /// из архитектурного плана, но путь не хардкодится здесь, чтобы тесты
    /// могли передать временный каталог.
    ///
    /// `base_image_path` — путь к уже скачанному и проверенному базовому
    /// образу для этого профиля; получение этого пути (кэш/скачивание) —
    /// явно вне рамок этого метода (см. документацию `AndroidProfile::resolve`)
    /// и пока не реализовано ни здесь, ни где-либо ещё в системе.
    ///
    /// `ovmf_vars_template` — путь к системному шаблону `OVMF_VARS`
    /// (например, `/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd`), который
    /// копируется в персональную копию инстанса, а не используется
    /// напрямую — каждый инстанс должен иметь свою копию, так как UEFI
    /// пишет в этот файл во время работы (boot order, Secure Boot keys и
    /// т.п.), и общий файл между инстансами привёл бы к гонкам/порче
    /// состояния друг друга.
    ///
    /// Если создание каталога инстанса, копирование шаблона `OVMF_VARS`
    /// или создание overlay-диска завершается ошибкой — инстанс не
    /// регистрируется в `Daemon` вообще (никакой частично созданной
    /// записи), но уже созданные на диске файлы (каталог, скопированный
    /// `VARS.fd`, если до него дошло) не удаляются — очистка частично
    /// созданного instance_dir в случае ошибки осознанно не реализована
    /// здесь: TODO для будущего шага, как и сам Factory Reset/удаление
    /// инстанса через `Daemon`.
    pub async fn create_android_instance(
        &self,
        profile: AndroidProfile,
        instance_name: String,
        base_image_path: PathBuf,
        instances_root: PathBuf,
        overlay_size_bytes: u64,
        ovmf_vars_template: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        let id = InstanceId::new();
        let instance_dir = instances_root.join(id.0.to_string());

        tokio::fs::create_dir_all(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        let ovmf_vars_path = instance_dir.join("VARS.fd");
        tokio::fs::copy(&ovmf_vars_template, &ovmf_vars_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: ovmf_vars_path.clone(),
                source,
            })?;

        let overlay = andler_disk::overlay::create_overlay(
            &instance_dir,
            &base_image_path,
            overlay_size_bytes,
        )
        .await?;

        let mut cfg = profile.resolve(
            instance_name,
            overlay.base_image_path,
            overlay.overlay_path,
            overlay_size_bytes,
            ovmf_vars_path,
        );
        // `resolve()` сама генерирует InstanceId (она ничего не знает про
        // instance_dir/overlay, которые мы уже создали под заранее выбранным
        // `id`) — перезаписываем тем `id`, под которым реально лежат файлы
        // на диске, иначе InstanceConfig.id разойдётся с именем каталога.
        cfg.id = id;

        self.create_instance(cfg).await
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
    use andler_core::{AndroidVersion, RootMode};
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

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_android_instance_resolves_profile_and_creates_overlay() {
        let dir = std::env::temp_dir().join("andler-daemon-test-android-e2e");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Базовый образ и шаблон OVMF_VARS — оба "настоящие" файлы на диске,
        // как и в реальной системе (только без реального содержимого UEFI
        // vars — для qemu-img create это не важно).
        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
            .await
            .unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            libndk: true,
            root: RootMode::None,
        };

        let id = daemon
            .create_android_instance(
                profile.clone(),
                "my-android".to_string(),
                base_image.clone(),
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Created);

        let instance_dir = instances_root.join(id.0.to_string());
        assert!(instance_dir.join("disk.qcow2").exists());
        assert!(instance_dir.join("VARS.fd").exists());

        let instances = daemon.instances.read().await;
        let record = instances.get(&id).unwrap();
        assert_eq!(record.config.id, id);
        assert_eq!(record.config.disk.base_image, Some(base_image));
        match &record.config.kind {
            InstanceKind::AndroidVm { android_profile } => {
                assert_eq!(*android_profile, profile);
            }
            InstanceKind::LinuxVm { .. } => panic!("expected AndroidVm"),
        }

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn create_android_instance_fails_when_base_image_missing() {
        let dir = std::env::temp_dir().join("andler-daemon-test-android-missing-base");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
            .await
            .unwrap();

        let missing_base = dir.join("does-not-exist.qcow2");
        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: true,
            libndk: false,
            root: RootMode::None,
        };

        let err = daemon
            .create_android_instance(
                profile,
                "my-android".to_string(),
                missing_base,
                instances_root,
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            DaemonError::Disk(andler_disk::DiskError::BackingFileNotFound(_))
        ));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
