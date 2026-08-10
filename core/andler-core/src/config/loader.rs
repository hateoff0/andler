//! TOML load/save for `instance.toml`, the single source of truth for
//! instance configuration (phase 1: config-source). Parsing is pure (string
//! in, config out) so `andler-core` stays I/O-free; the daemon supplies the
//! file bytes.

use crate::config::{migrate_schema, InstanceConfig};

/// Parses `instance.toml` content and runs schema migrations. Errors name
/// the offending line/field via the toml/serde context.
pub fn parse_instance_config_toml(input: &str) -> Result<InstanceConfig, String> {
    let mut cfg: InstanceConfig =
        toml::from_str(input).map_err(|e| format!("invalid instance.toml: {e}"))?;
    migrate_schema(&mut cfg)?;
    Ok(cfg)
}

/// Serializes a config back to the canonical `instance.toml` form the daemon
/// writes on every persist.
pub fn instance_config_to_toml(cfg: &InstanceConfig) -> Result<String, String> {
    toml::to_string_pretty(cfg).map_err(|e| format!("failed to serialize instance.toml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AudioConfig, BackendKind, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig,
        GpuConfig, InputConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
        CURRENT_SCHEMA_VERSION,
    };
    use std::path::PathBuf;

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "loader-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/loader.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            schema_version: CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/loader-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/firmware-VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[test]
    fn round_trip_preserves_config() {
        let cfg = sample_config();
        let toml = instance_config_to_toml(&cfg).unwrap();
        let back = parse_instance_config_toml(&toml).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_instance_config_toml("not toml [").is_err());
        assert!(parse_instance_config_toml("").is_err());
    }

    #[test]
    fn parse_rejects_unknown_future_schema() {
        let mut cfg = sample_config();
        cfg.schema_version = CURRENT_SCHEMA_VERSION + 1;
        let toml = instance_config_to_toml(&cfg).unwrap();
        assert!(parse_instance_config_toml(&toml).is_err());
    }

    #[test]
    fn parse_runs_migrations_from_v1() {
        let mut cfg = sample_config();
        cfg.schema_version = 1;
        let toml = instance_config_to_toml(&cfg).unwrap();
        let parsed = parse_instance_config_toml(&toml).unwrap();
        assert_eq!(parsed.schema_version, CURRENT_SCHEMA_VERSION);
    }
}
