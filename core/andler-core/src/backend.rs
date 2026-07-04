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
    /// VRAM used in bytes (AMD via sysfs `mem_info_vram_used`, None for other vendors).
    pub vram_used_bytes: Option<u64>,
    /// VRAM total in bytes (AMD via sysfs `mem_info_vram_total`, None for other vendors).
    pub vram_total_bytes: Option<u64>,
    /// GPU utilization % (AMD via sysfs `gpu_busy_percent`, None for other vendors).
    pub gpu_load_percent: Option<f32>,
}

/// Откуда взялась конкретная строка `LogLine` — на этом этапе только
/// stdout/stderr самого процесса гипервизора (для `andler-qemu`: то, что
/// уже перехватывает `process::QemuProcess::drain_to_tracing`). Не
/// включает структурированные события FSM `andler-daemon` (переходы
/// состояний, ошибки backend'а) и не включает гостевые логи (для этого
/// нужен бы отдельный serial-port/QMP-канал) — оба явно вне первой версии
/// `log_stream`, см. обсуждение в истории проекта (`StreamInstanceLogs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStreamSource {
    Stdout,
    Stderr,
}

/// Одна строка вывода процесса гипервизора, отдаваемая через
/// `log_stream`.
///
/// Намеренно не несёт `instance_id` — `log_stream` уже принимает
/// `&BackendHandle` одного конкретного инстанса, вызывающая сторона
/// (`andler-daemon`) и так знает, какому `InstanceId` соответствует этот
/// хэндл; дублировать это в каждой строке было бы избыточно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub source: LogStreamSource,
    pub line: String,
}

/// Метаданные снапшота, возвращаемые `snapshot_list`.
///
/// Конкретное наполнение определяется backend'ом (для QEMU — из
/// `query-block` с snapshot info). `tag` — пользовательский идентификатор,
/// заданный при создании снапшота.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub tag: String,
    pub id: String,
    /// Человекочитаемая дата/время создания (формат backend-специфичный).
    pub created_at: Option<String>,
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
    ///
    /// `timeout` — per-operation timeout override. If `None`, the backend
    /// uses its own default (e.g. `DiskConfig::snapshot_timeout_secs`).
    /// Для QEMU: `snapshot-save` job API (async). Дефолтная реализация
    /// возвращает `BackendError::NotImplemented` — backend без snapshot-
    /// поддержки не обязан переопределять этот метод.
    async fn snapshot(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, tag, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot",
        })
    }

    /// Восстанавливает инстанс из снапшота по тегу.
    ///
    /// `timeout` — per-operation timeout override. If `None`, the backend
    /// uses its own default.
    /// Инстанс должен быть запущен (`Running`/`Paused`) — QEMU snapshot-load
    /// выполняется через QMP, который требует живой процесс.
    async fn snapshot_restore(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, tag, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_restore",
        })
    }

    /// Удаляет снапшот по тегу.
    ///
    /// `timeout` — per-operation timeout override. If `None`, the backend
    /// uses its own default.
    /// Инстанс должен быть запущен (`Running`/`Paused`) — QEMU snapshot-delete
    /// выполняется через QMP, который требует живой процесс.
    /// Нельзя удалить снапшот, на который ссылается текущее состояние диска.
    async fn snapshot_delete(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, tag, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_delete",
        })
    }

    /// Возвращает список снапшотов инстанса.
    async fn snapshot_list(&self, handle: &BackendHandle) -> Result<Vec<SnapshotInfo>, BackendError> {
        let _ = handle;
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_list",
        })
    }

    /// Поток метрик ресурсов. Для backend'ов без реализации возвращает
    /// пустой поток, а не паникует — в отличие от остальных методов трейта,
    /// сигнатура этого метода не позволяет вернуть `Result`, поэтому
    /// "не реализовано" выражается как немедленно завершающийся поток.
    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>;

    /// Поток строк stdout/stderr процесса гипервизора (см. `LogLine`).
    ///
    /// Семантика та же, что у `metrics_stream`: для неизвестного хэндла
    /// или backend'а без реализации — немедленно завершающийся пустой
    /// поток, не ошибка и не паника (сигнатура не позволяет вернуть
    /// `Result` по тем же причинам, что и `metrics_stream`). Это
    /// сознательно отличается от того, как `pause`/`resume`/`status`
    /// обрабатывают отсутствующий хэндл (`BackendError::HandleNotFound`)
    /// — `log_stream` не запрашивает действие над процессом, который
    /// должен существовать, а наблюдает за тем, что есть; отсутствие
    /// живого процесса — основание для "сейчас нечего стримить", не для
    /// ошибки.
    ///
    /// Возможна, но не обязательна, история строк, появившихся до
    /// вызова `log_stream` — сам трейт не диктует, откуда её брать (это
    /// решение backend'а, например файл на диске), только то, что live
    /// tail с момента подключения — обязательный минимум для любой
    /// реализации. `andler-qemu`, например, отдаёт историю из
    /// `<instances_root>/<id>/qemu.log` перед живым хвостом (см.
    /// `QemuBackend::log_stream`) — до этого была версия, где `log_stream`
    /// сознательно отдавал только live-tail (см. историю этого
    /// комментария), это расширение, а не смена контракта: реализация,
    /// отдающая только live-tail, по-прежнему соответствует трейту.
    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine>;
}
