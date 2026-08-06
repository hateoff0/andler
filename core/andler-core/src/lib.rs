pub mod android_profile;
pub mod backend;
pub mod base_image;
pub mod clone;
pub mod config;
pub mod error;
pub mod fsm;
pub mod paths;

// Tests that mutate ANDLER_HOME must serialize on this single lock — a
// per-module lock does not protect against a parallel module's test reading
// a half-set environment variable.
#[cfg(test)]
pub mod test_lock {
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

pub use android_profile::{AndroidBootMode, AndroidProfile, AndroidVersion, ArmTranslator};
pub use backend::{
    BackendHandle, BackendStatus, HypervisorBackend, LogLine, LogStreamSource, ResourceMetrics,
    SnapshotInfo,
};
pub use clone::CloneMode;
pub use config::{
    AudioBackend, AudioConfig, AudioDevice, BackendKind, CdromBus, CpuConfig, CpuPriority,
    DiskConfig, DiskFormat, DisplayConfig, DisplayEngine, FirmwareConfig, GpuConfig, InputConfig,
    InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NatBackend, NetworkConfig, NetworkMode,
    PointerMode, RenderBackend, Resolution, INSTANCE_ID_HEX_LEN,
};
pub use error::{BackendError, FsmError};
pub use fsm::{InstanceEvent, InstanceState};
