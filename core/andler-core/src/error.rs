use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("operation `{operation}` is not implemented by backend `{backend}`")]
    NotImplemented {
        backend: &'static str,
        operation: &'static str,
    },

    #[error("instance handle `{0}` not found by backend")]
    HandleNotFound(String),

    #[error("backend I/O error: {0}")]
    Io(String),

    #[error("invalid configuration for backend `{backend}`: {reason}")]
    InvalidConfig {
        backend: &'static str,
        reason: String,
    },

    #[error("hypervisor process for this instance is no longer running")]
    ProcessNotRunning,
}

#[derive(Debug, Error)]
pub enum FsmError {
    #[error("transition `{event:?}` is not allowed from state `{from:?}`")]
    InvalidTransition {
        from: crate::fsm::InstanceState,
        event: crate::fsm::InstanceEvent,
    },
}
