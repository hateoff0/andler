mod audio;
mod cdrom;
mod cpu;
mod disk;
mod display;
mod draft;
mod firmware;
mod gpu;
mod input;
mod instance;
mod keypath;
mod loader;
mod memory;
mod network;

pub use audio::{AudioBackend, AudioConfig, AudioDevice};
pub use cdrom::CdromBus;
pub use cpu::{CpuConfig, CpuPriority};
pub use disk::{DiskConfig, DiskFormat};
pub use display::{DisplayConfig, DisplayEngine, Resolution};
pub use draft::{
    ConfigDraft, DiskDraft, DiskSource, DraftKind, FirmwareDraft, HostFirmware, DEFAULT_DISK_GIB,
    DEFAULT_OVERLAY_GIB,
};
pub use firmware::FirmwareConfig;
pub use gpu::{GpuConfig, RenderBackend};
pub use input::{InputConfig, PointerMode};
pub use instance::{BackendKind, InstanceConfig, InstanceId, InstanceKind, INSTANCE_ID_HEX_LEN};
pub use keypath::{
    config_keys, diff_configs, get_key, is_live_key, set_key, ConfigKey, ConfigKeyError,
};
pub use loader::{instance_config_to_toml, parse_instance_config_toml};
pub use memory::MemoryConfig;
pub use network::{NatBackend, NetworkConfig, NetworkMode, PortForward, PortForwardProtocol};

/// Current on-disk config schema version. Bump it when `InstanceConfig` gains
/// a field that old files must migrate; add the migration step to
/// `migrate_schema()` in the same commit.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Applies all pending schema migrations to a config loaded from disk,
/// advancing `schema_version` to `CURRENT_SCHEMA_VERSION`. Errors on a version
/// newer than the daemon knows or one with no migration path — an explicit
/// failure, never a silent reinterpretation of unknown fields.
pub fn migrate_schema(cfg: &mut InstanceConfig) -> Result<(), String> {
    if cfg.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "config schema version {} is newer than this andlerd supports ({CURRENT_SCHEMA_VERSION}); upgrade andlerd first",
            cfg.schema_version
        ));
    }
    while cfg.schema_version < CURRENT_SCHEMA_VERSION {
        migrate_one_step(cfg)?;
    }
    Ok(())
}

fn migrate_one_step(cfg: &mut InstanceConfig) -> Result<(), String> {
    match cfg.schema_version {
        // v1 -> v2: register the first real migration here and bump
        // CURRENT_SCHEMA_VERSION; until then every loaded config is already
        // current.
        1 => {}
        version => {
            return Err(format!(
                "unsupported config schema version {version} (daemon knows up to {CURRENT_SCHEMA_VERSION})"
            ))
        }
    }
    cfg.schema_version += 1;
    Ok(())
}
