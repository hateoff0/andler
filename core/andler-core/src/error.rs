//! Общие типы ошибок домена `andler-core`.
//!
//! `BackendError` — ошибки конкретной реализации `HypervisorBackend`
//! (см. `backend.rs`). `FsmError` — ошибки переходов состояний (см. `fsm.rs`).
//!
//! Контракт `BackendError::NotImplemented`: любой backend, который не умеет
//! выполнить операцию на текущем этапе разработки (например, `andler-vmm`
//! целиком, либо `RenderBackend::Passthrough` внутри `andler-qemu`), обязан
//! вернуть эту ошибку, а не вызвать `todo!()`/`unimplemented!()`. Так
//! `andler-daemon` может транслировать её в штатный gRPC-статус `UNIMPLEMENTED`
//! вместо падения процесса.
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.1 и §9 (п.3).

use thiserror::Error;

/// Ошибки, которые может вернуть любая реализация `HypervisorBackend`.
#[derive(Debug, Error)]
pub enum BackendError {
    /// Операция не реализована данным backend'ом на текущем этапе.
    /// Это штатный, ожидаемый результат для незавершённых backend'ов
    /// (`andler-vmm`) или зарезервированных функций (`RenderBackend::Passthrough`),
    /// а не признак внутренней ошибки.
    #[error("operation `{operation}` is not implemented by backend `{backend}`")]
    NotImplemented {
        backend: &'static str,
        operation: &'static str,
    },

    /// Хэндл указывает на инстанс, которого backend не знает (например,
    /// процесс уже завершился вне нашего контроля).
    #[error("instance handle `{0}` not found by backend")]
    HandleNotFound(String),

    /// Ошибка запуска или взаимодействия с базовым процессом/гипервизором
    /// (например, QEMU не запустился, QMP-сокет недоступен).
    #[error("backend I/O error: {0}")]
    Io(String),

    /// Конфигурация инстанса некорректна для данного backend'а
    /// (например, запрошен `RenderBackend`, который backend не поддерживает —
    /// см. `HypervisorBackend::supported_render_backends`).
    #[error("invalid configuration for backend `{backend}`: {reason}")]
    InvalidConfig {
        backend: &'static str,
        reason: String,
    },
}

/// Ошибки переходов конечного автомата состояний инстанса.
#[derive(Debug, Error)]
pub enum FsmError {
    /// Запрошенный переход не разрешён из текущего состояния.
    #[error("transition `{event:?}` is not allowed from state `{from:?}`")]
    InvalidTransition {
        from: crate::fsm::InstanceState,
        event: crate::fsm::InstanceEvent,
    },
}
