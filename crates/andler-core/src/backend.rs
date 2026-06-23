//! Абстракция backend'а гипервизора.
//!
//! `HypervisorBackend` — единственная точка, через которую `andler-daemon`
//! управляет виртуальными машинами. `andler-qemu` реализует его поверх
//! процесса QEMU; `andler-vmm` — пустой каркас под rust-vmm (см. README
//! этого крейта и docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.1, §9).
//!
//! Контракт для незавершённых backend'ов: любой метод, который backend не
//! умеет выполнить на данном этапе, возвращает
//! `BackendError::NotImplemented`, а не паникует. Это позволяет
//! зарегистрировать backend в реестре `andler-daemon` до его полной
//! готовности и получить штатную gRPC-ошибку `UNIMPLEMENTED` вместо краша
//! демона.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::config::InstanceConfig;
use crate::error::BackendError;
use crate::fsm::InstanceState;

/// Непрозрачный идентификатор запущенного инстанса с точки зрения backend'а.
///
/// Для `andler-qemu` это, например, PID процесса QEMU + путь к QMP-сокету;
/// конкретное содержимое — деталь реализации конкретного backend'а, поэтому
/// тип объявлен здесь как непрозрачная обёртка над строкой, а не enum с
/// вариантами всех backend'ов (это привязало бы `andler-core` к деталям
/// `andler-qemu`/`andler-vmm`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendHandle(pub String);

/// Снимок состояния инстанса с точки зрения backend'а — то, что backend
/// реально наблюдает (например, через QMP `query-status`), в отличие от
/// `InstanceState` из `fsm.rs`, которое отражает логику переходов на
/// стороне `andler-daemon`. В норме они синхронизированы, но `status()`
/// существует именно для проверки этой синхронизации (например, если
/// процесс QEMU упал, а демон ещё не узнал об этом).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub state: InstanceState,
    /// Человекочитаемая причина, если state расходится с ожиданиями
    /// (например, "process exited with code 1").
    pub detail: Option<String>,
}

/// Метрики ресурсов инстанса, отдаваемые через `metrics_stream`.
///
/// Источники конкретных полей различаются (host-side `/proc`, QMP polling,
/// в перспективе guest-agent) — см. docs/architecture/CORE_ARCHITECTURE_PLAN.md,
/// §6.1.1. На уровне трейта это не видно намеренно: вызывающая сторона не
/// должна знать, как backend получил число.
#[derive(Debug, Clone, Default)]
pub struct ResourceMetrics {
    pub cpu_percent: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub disk_read_bytes_per_sec: Option<u64>,
    pub disk_write_bytes_per_sec: Option<u64>,
    pub net_rx_bytes_per_sec: Option<u64>,
    pub net_tx_bytes_per_sec: Option<u64>,
}

/// Единый интерфейс для разных гипервизоров (QEMU сейчас, rust-vmm в
/// перспективе). `andler-daemon` работает только через этот трейт и не
/// импортирует `andler-qemu`/`andler-vmm` напрямую за пределами реестра
/// backend'ов.
#[async_trait]
pub trait HypervisorBackend: Send + Sync {
    /// Машиночитаемое имя backend'а (`"qemu"`, `"vmm"`) — используется в
    /// `BackendError::NotImplemented` и для выбора backend'а по
    /// `BackendKind` из запроса.
    fn name(&self) -> &'static str;

    /// Какие `RenderBackend` (см. `config::gpu`) этот backend умеет
    /// обслужить. Используется для ранней валидации конфигурации до
    /// попытки `spawn` — если backend в принципе не поддерживает запрошенный
    /// render backend, `spawn` должен вернуть `InvalidConfig`, а не пытаться
    /// запуститься и упасть на полпути.
    fn supported_render_backends(&self) -> &[crate::config::RenderBackend];

    /// Запускает инстанс согласно конфигурации. Возвращает хэндл для
    /// дальнейших операций.
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>;

    /// Приостанавливает работающий инстанс.
    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError>;

    /// Возобновляет приостановленный инстанс.
    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError>;

    /// Останавливает инстанс. `graceful = true` — попытка штатного
    /// завершения (ACPI shutdown через QMP); `false` — принудительное
    /// завершение процесса.
    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>;

    /// Текущий статус инстанса с точки зрения backend'а.
    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>;

    /// Создаёт снимок состояния инстанса с заданным тегом.
    async fn snapshot(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>;

    /// Поток метрик ресурсов. Для backend'ов без реализации возвращает
    /// пустой поток, а не паникует — в отличие от остальных методов трейта,
    /// сигнатура этого метода не позволяет вернуть `Result`, поэтому
    /// "не реализовано" выражается как немедленно завершающийся поток.
    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>;
}
