use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    AudioConfig, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
    InputConfig, MemoryConfig, NetworkConfig,
};
use crate::android_profile::AndroidProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub Uuid);

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl InstanceId {
    pub fn new() -> Self {
        InstanceId(Uuid::new_v4())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    Qemu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceKind {
    LinuxVm {
        iso_path: PathBuf,

        cdrom_bus: CdromBus,
    },

    AndroidVm {
        android_profile: AndroidProfile,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub id: InstanceId,
    pub name: String,
    pub kind: InstanceKind,
    pub backend: BackendKind,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,
    pub disk: DiskConfig,
    pub display: DisplayConfig,
    pub gpu: GpuConfig,
    pub network: NetworkConfig,

    pub firmware: FirmwareConfig,
    pub audio: AudioConfig,
    pub input: InputConfig,
}

impl InstanceConfig {
    /// Rejects degenerate values (zero cores/memory/disk/display size) that the
    /// interactive wizard already guards against — but `andler config edit`,
    /// `andler config set`, and instance-file-based creation all bypass the wizard
    /// entirely and go straight to the daemon, which previously accepted these with
    /// no checks at all. QEMU rejects most of them too, but only at spawn time, with
    /// a much less clear error than catching them here up front.
    pub fn validate(&self) -> Result<(), String> {
        if self.cpu.cores == 0 {
            return Err("cpu.cores must be at least 1".to_string());
        }
        if self.memory.size_bytes == 0 {
            return Err("memory.size_bytes must be greater than 0".to_string());
        }
        if self.disk.size_bytes == 0 {
            return Err("disk.size_bytes must be greater than 0".to_string());
        }
        if self.display.resolution.width == 0 || self.display.resolution.height == 0 {
            return Err(format!(
                "display resolution must be non-zero (got {}x{})",
                self.display.resolution.width, self.display.resolution.height
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::Resolution;
    use super::*;

    #[test]
    fn instance_id_is_unique() {
        let a = InstanceId::new();
        let b = InstanceId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn config_round_trips_through_serde_json() {
        let cfg = InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/cachyos.iso"),
                cdrom_bus: CdromBus::VirtioScsi,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        };

        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: InstanceConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, restored);
    }

    fn sample_valid_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/cachyos.iso"),
                cdrom_bus: CdromBus::VirtioScsi,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[test]
    fn validate_accepts_reference_default_config() {
        assert!(sample_valid_config().validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_cpu_cores() {
        let mut cfg = sample_valid_config();
        cfg.cpu.cores = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_memory() {
        let mut cfg = sample_valid_config();
        cfg.memory.size_bytes = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_disk_size() {
        let mut cfg = sample_valid_config();
        cfg.disk.size_bytes = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_display_resolution() {
        let mut cfg = sample_valid_config();
        cfg.display.resolution = Resolution::new(0, 1080);
        assert!(cfg.validate().is_err());

        let mut cfg2 = sample_valid_config();
        cfg2.display.resolution = Resolution::new(1920, 0);
        assert!(cfg2.validate().is_err());
    }
}
