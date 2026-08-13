pub mod android_profile;
pub mod backend;
pub mod base_image;
pub mod clone;
pub mod config;
pub mod disk_chain;
pub mod error;
pub mod events;
pub mod fsm;
pub mod package_manager;
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
    BackendHandle, BackendStatus, GuestExecOutput, HypervisorBackend, LogLine, LogStreamSource,
    ResourceMetrics, SnapshotInfo,
};
pub use clone::CloneMode;
pub use config::{
    config_keys, diff_configs, get_key, instance_config_to_toml, is_live_key,
    parse_instance_config_toml, set_key, ConfigKey, ConfigKeyError,
};
pub use config::{migrate_schema, CURRENT_SCHEMA_VERSION};
pub use config::{
    AudioBackend, AudioConfig, AudioDevice, BackendKind, CdromBus, CpuConfig, CpuPriority,
    DiskConfig, DiskFormat, DisplayConfig, DisplayEngine, FirmwareConfig, GpuConfig, InputConfig,
    InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NatBackend, NetworkConfig, NetworkMode,
    PointerMode, RenderBackend, Resolution, INSTANCE_ID_HEX_LEN,
};
pub use disk_chain::{DiskChain, DiskLayer, LayerKind};
pub use error::{BackendError, FsmError};
pub use events::{
    DaemonEvent, EventKind, EventLogLevel, GuestReadinessLevel, OpId, Operation, OperationKind,
    OperationState, QmpEvent,
};
pub use fsm::{InstanceEvent, InstanceState};
pub use package_manager::PackageManager;
pub mod sizes;
