//! Доменная модель ANDLER: конфигурация инстансов, конечный автомат
//! состояний и абстракция backend'а гипервизора.
//!
//! Этот крейт не должен зависеть ни от одного другого крейта workspace'а —
//! см. README.md этой папки. Архитектурное обоснование:
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md.

pub mod android_profile;
pub mod backend;
pub mod clone;
pub mod config;
pub mod error;
pub mod fsm;
pub mod paths;

pub use android_profile::{AndroidProfile, AndroidVersion, RootMode};
pub use backend::{
    BackendHandle, BackendStatus, HypervisorBackend, LogLine, LogStreamSource, ResourceMetrics,
    SnapshotInfo,
};
pub use clone::CloneMode;
pub use config::{
    AudioBackend, AudioConfig, BackendKind, CdromBus, CpuConfig, CpuPriority, DiskConfig,
    DiskFormat, DisplayConfig, DisplayEngine, FirmwareConfig, GpuConfig, InputConfig,
    InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig, NetworkMode,
    RenderBackend, Resolution,
};
pub use error::{BackendError, FsmError};
pub use fsm::{InstanceEvent, InstanceState};
