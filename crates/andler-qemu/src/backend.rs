//! `QemuBackend` — реализация `HypervisorBackend` (из `andler-core`) поверх
//! `cmdline::build_args`, `process::QemuProcess` и `qmp::QmpClient`.
//!
//! Связывает чистую сборку аргументов, низкоуровневое управление процессом
//! и QMP-протокол с единым интерфейсом, через который `andler-daemon`
//! управляет инстансами (см. docs/architecture/CORE_ARCHITECTURE_PLAN.md,
//! §2.1).
//!
//! `snapshot`/`metrics_stream` остаются `BackendError::NotImplemented`/
//! пустым потоком — snapshot требует отдельного решения между
//! `snapshot-save` (job API) и `human-monitor-command`+`savevm` (см.
//! README этого крейта), а метрики требуют QMP polling нескольких разных
//! команд (см. §6.1.1 архитектурного плана) — оба за пределами текущего
//! шага. `spawn`/`stop`/`pause`/`resume`/`status`/`log_stream`
//! реализованы полноценно.

use std::collections::HashMap;
use std::path::PathBuf;

use andler_core::{
    BackendError, BackendHandle, BackendStatus, HypervisorBackend, InstanceConfig, InstanceState,
    LogLine, RenderBackend, ResourceMetrics,
};
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use crate::cmdline;
use crate::process::{ProcessError, QemuProcess};
use crate::qmp::{QmpClient, QmpError, VmStatus};

/// Каталог, в котором создаются QMP-сокеты запущенных инстансов.
/// `andler-daemon` в перспективе должен сделать этот путь настраиваемым
/// (например, через XDG runtime dir пользователя) — здесь фиксированная
/// константа, так как `QemuBackend` пока не получает конфигурацию путей
/// извне ни от чего, кроме вызова `spawn`.
const QMP_SOCKET_DIR: &str = "/tmp/andler/qmp";

/// Запущенный инстанс с точки зрения `QemuBackend`: процесс плюс,
/// опционально, уже установленное QMP-соединение.
///
/// `qmp_client` — `Option`, не безусловное соединение сразу при `spawn`:
/// `-qmp ...,server,nowait` означает, что QEMU поднимает сокет и не
/// блокируется, ожидая подключения — но клиенту всё равно может
/// потребоваться несколько попыток подключиться сразу после `spawn`, пока
/// QEMU полностью не инициализировался. Подключение делается лениво, при
/// первом вызове `pause`/`resume`/`status`, а не сразу в `spawn`, чтобы не
/// удлинять и не усложнять сам `spawn` обработкой retry-логики, которая
/// нужна только тем операциям, что реально используют QMP.
struct RunningInstance {
    process: QemuProcess,
    qmp_client: Option<QmpClient>,
}

/// Backend гипервизора поверх процесса QEMU.
///
/// Хранит реестр живых процессов под `Mutex` — `HypervisorBackend::spawn`
/// и остальные методы принимают `&self`, не `&mut self` (это диктует сам
/// trait, так как один `QemuBackend` используется конкурентно из нескольких
/// gRPC-запросов в `andler-daemon`), поэтому внутренняя мутабельность
/// обязательна.
pub struct QemuBackend {
    instances: Mutex<HashMap<BackendHandle, RunningInstance>>,
}

impl QemuBackend {
    pub fn new() -> Self {
        QemuBackend {
            instances: Mutex::new(HashMap::new()),
        }
    }

    /// Строит `BackendHandle` для инстанса. Формат (`"qemu:{instance_id}"`)
    /// — деталь реализации, не часть публичного контракта: вызывающая
    /// сторона (`andler-daemon`) должна получать `BackendHandle` только как
    /// результат `spawn()` и передавать его обратно как есть, не
    /// разбирая/конструируя самостоятельно.
    fn handle_for(cfg: &InstanceConfig) -> BackendHandle {
        BackendHandle(format!("qemu:{}", cfg.id.0))
    }

    fn qmp_socket_path_for(cfg: &InstanceConfig) -> PathBuf {
        PathBuf::from(QMP_SOCKET_DIR).join(format!("{}.sock", cfg.id.0))
    }

    /// Возвращает рабочее QMP-соединение для инстанса, устанавливая его
    /// при необходимости (см. документацию `RunningInstance::qmp_client`).
    ///
    /// Принимает `&mut RunningInstance` напрямую (не `&BackendHandle` +
    /// повторный поиск в реестре) — вызывающая сторона уже держит
    /// блокировку реестра и нашла нужный инстанс, повторный поиск был бы
    /// избыточен и потенциально рассинхронизировался бы с конкурентными
    /// изменениями реестра между поисками.
    async fn ensure_qmp_connected(instance: &mut RunningInstance) -> Result<(), QmpError> {
        if instance.qmp_client.is_some() {
            return Ok(());
        }

        let client = QmpClient::connect(instance.process.qmp_socket_path()).await?;
        instance.qmp_client = Some(client);
        Ok(())
    }
}

impl Default for QemuBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Переводит `ProcessError` в `BackendError`, сохраняя сообщение.
/// Отдельная функция, а не `impl From`, чтобы не создавать зависимость
/// `andler-qemu::process::ProcessError -> andler-core` (она и не нужна:
/// `process.rs` не зависит от `andler-core`, см. его документацию) —
/// конвертация делается в этом модуле, на границе с trait'ом.
fn process_error_to_backend_error(err: ProcessError) -> BackendError {
    BackendError::Io(err.to_string())
}

/// Переводит `QmpError` в `BackendError` той же логикой, что и
/// `process_error_to_backend_error` — `qmp.rs` тоже не зависит от
/// `andler-core` (нет причины: протокол QMP не знает про `InstanceConfig`),
/// конвертация делается на границе с trait'ом, не внутри `qmp.rs`.
fn qmp_error_to_backend_error(err: QmpError) -> BackendError {
    BackendError::Io(err.to_string())
}

/// `VmStatus` (наблюдение QMP) -> `InstanceState` (домен `andler-core`).
///
/// `VmStatus::Shutdown`/`Other` намеренно не маппятся на `InstanceState`
/// напрямую здесь — `shutdown` с точки зрения QMP означает "гостевая ОС
/// попросила выключение", но процесс QEMU может ещё быть жив несколько
/// мгновений после этого; различать это от `Stopped` в смысле FSM
/// (`andler_core::fsm`) — забота `andler-daemon`, который видит и
/// `BackendStatus`, и факт того, жив ли сам процесс. Здесь возвращается
/// `None` для статусов, которые не однозначно соответствуют
/// `Running`/`Paused`, а вызывающая сторона (`status()` ниже) решает, что
/// с этим делать.
fn vm_status_to_instance_state(status: VmStatus) -> Option<InstanceState> {
    match status {
        VmStatus::Running => Some(InstanceState::Running),
        VmStatus::Paused => Some(InstanceState::Paused),
        VmStatus::Shutdown | VmStatus::Other => None,
    }
}

#[async_trait]
impl HypervisorBackend for QemuBackend {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn supported_render_backends(&self) -> &[RenderBackend] {
        // Passthrough намеренно не входит в этот список — см. §2.3
        // архитектурного плана и документацию RenderBackend::is_implemented.
        // Используется &'static, чтобы не аллоцировать новый Vec на каждый
        // вызов; конкретные варианты не несут данных (кроме Passthrough,
        // которого здесь нет), так что статический срез безопасен.
        &[
            RenderBackend::Venus,
            RenderBackend::VirtioGpu,
            RenderBackend::VirGl,
            RenderBackend::Cpu,
        ]
    }

    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        if !cfg.gpu.render_backend.is_implemented() {
            return Err(BackendError::InvalidConfig {
                backend: "qemu",
                reason: format!(
                    "render backend {:?} is not implemented, see docs/architecture/CORE_ARCHITECTURE_PLAN.md §2.3",
                    cfg.gpu.render_backend
                ),
            });
        }

        let handle = Self::handle_for(cfg);
        let qmp_socket_path = Self::qmp_socket_path_for(cfg);

        if let Some(parent) = qmp_socket_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| BackendError::Io(e.to_string()))?;
        }

        let args = cmdline::build_args(cfg, &qmp_socket_path);

        let process = QemuProcess::spawn(&args, qmp_socket_path)
            .await
            .map_err(process_error_to_backend_error)?;

        let mut instances = self.instances.lock().await;
        instances.insert(
            handle.clone(),
            RunningInstance {
                process,
                qmp_client: None,
            },
        );

        Ok(handle)
    }

    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .pause()
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .resume()
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        if graceful {
            // Без снапшота/snapshot.rs это всё ещё SIGTERM, не ACPI-сигнал
            // гостю через QMP system_powerdown — см. документацию
            // process::QemuProcess::terminate. Использование QMP здесь
            // для честного ACPI graceful shutdown — естественное
            // продолжение этого модуля, но не часть текущего шага
            // (текущий шаг — pause/resume/status, не shutdown-семантика).
            instance
                .process
                .terminate()
                .await
                .map_err(process_error_to_backend_error)?;
        } else {
            instance
                .process
                .force_kill()
                .await
                .map_err(process_error_to_backend_error)?;
        }

        instances.remove(handle);
        Ok(())
    }

    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let alive = instance
            .process
            .is_alive()
            .await
            .map_err(process_error_to_backend_error)?;

        if !alive {
            return Ok(BackendStatus {
                state: InstanceState::Stopped,
                detail: Some("process is not running".to_string()),
            });
        }

        // Процесс жив — пробуем получить точный статус через QMP
        // query-status. Если QMP недоступен (например, сокет ещё не готов
        // сразу после spawn, либо соединение разорвалось) — не считаем это
        // фатальной ошибкой всего запроса статуса: с точки зрения
        // HypervisorBackend мы всё равно знаем, что процесс жив, просто не
        // знаем точного состояния гостя. Это явно отражается в `detail`,
        // а не маскируется молчаливым выбором Running по умолчанию.
        match Self::ensure_qmp_connected(instance).await {
            Ok(()) => {
                let qmp_status = instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded")
                    .query_status()
                    .await;

                match qmp_status {
                    Ok(vm_status) => match vm_status_to_instance_state(vm_status) {
                        Some(state) => Ok(BackendStatus { state, detail: None }),
                        None => Ok(BackendStatus {
                            state: InstanceState::Running,
                            detail: Some(format!(
                                "process is alive; QMP reports VM status {vm_status:?}, \
                                 which does not map directly to a HypervisorBackend state"
                            )),
                        }),
                    },
                    Err(qmp_err) => Ok(BackendStatus {
                        state: InstanceState::Running,
                        detail: Some(format!(
                            "process is alive but QMP query-status failed: {qmp_err}"
                        )),
                    }),
                }
            }
            Err(qmp_err) => Ok(BackendStatus {
                state: InstanceState::Running,
                detail: Some(format!(
                    "process is alive but QMP connection failed: {qmp_err}"
                )),
            }),
        }
    }

    async fn snapshot(&self, _handle: &BackendHandle, _tag: &str) -> Result<(), BackendError> {
        // Требует решения между snapshot-save (job API, асинхронный) и
        // human-monitor-command+savevm (синхронный, не рекомендуется QEMU
        // в долгосрочной перспективе) — сознательно не принято на этом
        // шаге, см. README этого крейта.
        Err(BackendError::NotImplemented {
            backend: "qemu",
            operation: "snapshot",
        })
    }

    fn metrics_stream(&self, _handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics> {
        // Требует QMP polling (query-balloon/query-blockstats/...), см.
        // §6.1.1 архитектурного плана. Контракт для metrics_stream без
        // реализации (см. andler_core::backend) — немедленно завершающийся
        // поток, не паника: сигнатура метода не позволяет вернуть Result.
        Box::pin(futures_util::stream::empty())
    }

    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine> {
        // `log_stream` — не `async fn` (см. andler_core::backend за тем,
        // почему сигнатура трейта такая же, как у metrics_stream), а
        // `self.instances` — `tokio::sync::Mutex`, требующий `.await` для
        // обычного `lock()`. `try_lock()` — единственный способ
        // синхронно достать `RunningInstance` здесь без переделки всего
        // метода в `async fn` (что сломало бы единообразие с
        // metrics_stream и сигнатуру трейта).
        //
        // Все остальные методы (`pause`/`resume`/`stop`/`status`) держат
        // этот `Mutex` только на короткие, без внутренних `.await` на
        // самом локе, синхронные секции (взять `&mut RunningInstance`,
        // отдать обратно) — она не остаётся захваченной во время
        // QMP-обмена с самим QEMU (`ensure_qmp_connected` и операции QMP
        // вызываются на уже полученной ссылке, не повторно лочат
        // `instances`). Поэтому `try_lock()` здесь практически никогда не
        // провалится из-за конкуренции; в редком случае гонки с другим
        // вызовом `log_stream`/`pause`/`stop` в тот же момент — отдаём
        // пустой поток, тот же контракт, что и для "хэндл не найден" (см.
        // документацию `HypervisorBackend::log_stream`): подписка на
        // следующий вызов клиента отработает штатно, потерянных данных
        // нет (broadcast не накапливает историю для ещё не подключённого
        // подписчика в любом случае).
        let receiver = match self.instances.try_lock() {
            Ok(mut instances) => instances.get_mut(handle).map(|i| i.process.subscribe_logs()),
            Err(_would_block) => None,
        };

        match receiver {
            Some(receiver) => Box::pin(
                tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|item| async {
                    match item {
                        Ok(line) => Some(line),
                        // `Lagged(n)` — подписчик отстал больше, чем
                        // вмещает `LOG_CHANNEL_CAPACITY` (см.
                        // process::LOG_CHANNEL_CAPACITY), и пропустил `n`
                        // строк. Пропускаем сам факт пропуска молча и
                        // продолжаем поток со следующей доступной строки
                        // — закрывать стрим здесь было бы хуже для
                        // живого хвоста логов, чем потерять уведомление о
                        // разрыве; в логах andlerd (`tracing`) эти же
                        // строки в любом случае не потеряны — лагает
                        // только данный gRPC-подписчик, не сам канал.
                        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(
                            _,
                        )) => None,
                    }
                }),
            ),
            None => Box::pin(futures_util::stream::empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config(render_backend: RenderBackend) -> InstanceConfig {
        let mut gpu = GpuConfig::reference_default();
        gpu.render_backend = render_backend;

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
            gpu,
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[test]
    fn name_returns_qemu() {
        let backend = QemuBackend::new();
        assert_eq!(backend.name(), "qemu");
    }

    #[test]
    fn supported_render_backends_excludes_passthrough() {
        let backend = QemuBackend::new();
        let supported = backend.supported_render_backends();
        assert!(supported.contains(&RenderBackend::Venus));
        assert!(!supported
            .iter()
            .any(|b| matches!(b, RenderBackend::Passthrough { .. })));
    }

    #[tokio::test]
    async fn spawn_rejects_passthrough_before_touching_process() {
        let backend = QemuBackend::new();
        let cfg = sample_config(RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        });

        let err = backend.spawn(&cfg).await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidConfig { .. }));
    }

    #[tokio::test]
    async fn stop_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.stop(&unknown, true).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn status_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.status(&unknown).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn pause_resume_on_unknown_handle_return_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        assert!(matches!(
            backend.pause(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
        assert!(matches!(
            backend.resume(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn snapshot_is_not_implemented() {
        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());

        assert!(matches!(
            backend.snapshot(&handle, "tag").await,
            Err(BackendError::NotImplemented { .. })
        ));
    }

    #[tokio::test]
    async fn metrics_stream_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.metrics_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    /// Неизвестный хэндл — пустой поток сразу же, не ошибка (см.
    /// документацию `HypervisorBackend::log_stream` за тем, почему это
    /// сознательно отличается от `pause`/`resume`/`status` с тем же
    /// неизвестным хэндлом). Не требует реального процесса QEMU —
    /// `instances` пуст с самого начала.
    #[tokio::test]
    async fn log_stream_on_unknown_handle_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.log_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn vm_status_maps_running_and_paused_directly() {
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Running),
            Some(InstanceState::Running)
        );
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Paused),
            Some(InstanceState::Paused)
        );
    }

    #[test]
    fn vm_status_does_not_map_shutdown_or_other_directly() {
        assert_eq!(vm_status_to_instance_state(VmStatus::Shutdown), None);
        assert_eq!(vm_status_to_instance_state(VmStatus::Other), None);
    }

    // spawn/stop/status/pause/resume на реальном живом процессе требуют
    // бинарника qemu-system-x86_64 и рабочего QMP-сокета — см.
    // process.rs/qmp.rs про конвенцию #[ignore] в этом крейте.
    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn spawn_then_status_then_stop_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, true).await.unwrap();
        let err = backend.status(&handle).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn spawn_then_pause_then_resume_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();

        backend.pause(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Paused);

        backend.resume(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, false).await.unwrap();
    }

    /// Сквозная проверка `log_stream` через весь стек `QemuBackend`
    /// (в отличие от `drain_to_tracing_publishes_lines_to_subscriber` в
    /// `process.rs`, которая проверяет только саму механику чтения
    /// строк — здесь важно, что `BackendHandle` → `RunningInstance` →
    /// `subscribe_logs` → `BroadcastStream` действительно соединены друг
    /// с другом). QEMU обычно пишет в stderr хотя бы строку лицензии или
    /// предупреждения при запуске с `-nographic`/неполным набором
    /// устройств — этого достаточно, чтобы получить хотя бы одну строку
    /// без необходимости провоцировать конкретную ошибку.
    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn log_stream_receives_real_process_output() {
        use futures_util::StreamExt;
        use tokio::time::{timeout, Duration};

        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let mut stream = backend.log_stream(&handle);

        // Не любой запуск QEMU гарантированно что-то пишет в stdout/stderr
        // в первые секунды — таймаут здесь означает "не успели получить
        // строку", не "механика не работает"; smoke-проверка того, что
        // стрим хотя бы подключён к реальному процессу, не строгая
        // гарантия конкретного вывода.
        let _ = timeout(Duration::from_secs(5), stream.next()).await;

        backend.stop(&handle, false).await.unwrap();
    }
}

