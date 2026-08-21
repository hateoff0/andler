use std::path::PathBuf;

use rand::RngExt;
use serde::{Deserialize, Serialize};

use super::{
    AudioConfig, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
    InputConfig, MemoryConfig, NetworkConfig, RenderBackend,
};
use crate::android_profile::AndroidProfile;

pub const INSTANCE_ID_HEX_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId([u8; 32]);

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl InstanceId {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        Self(bytes)
    }
}

impl serde::Serialize for InstanceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for InstanceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidInstanceId;

impl std::fmt::Display for InvalidInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "instance ID must be {INSTANCE_ID_HEX_LEN} hex chars")
    }
}

impl std::str::FromStr for InstanceId {
    type Err = InvalidInstanceId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != INSTANCE_ID_HEX_LEN {
            return Err(InvalidInstanceId);
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16).ok_or(InvalidInstanceId)?;
            let lo = (pair[1] as char).to_digit(16).ok_or(InvalidInstanceId)?;
            out[i] = ((hi as u8) << 4) | lo as u8;
        }
        Ok(Self(out))
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

    /// On-disk schema version (see `CURRENT_SCHEMA_VERSION`); missing in files
    /// that predate schema versioning, which read back as v1.
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,
    pub disk: DiskConfig,
    pub display: DisplayConfig,
    pub gpu: GpuConfig,
    pub network: NetworkConfig,

    /// Extra block devices attached while the instance is running; persisted and
    /// re-attached at boot. Not covered by internal snapshots (primary disk only).
    #[serde(default)]
    pub extra_disks: Vec<DiskConfig>,

    /// Extra NICs attached while the instance is running; persisted and re-created
    /// at boot (host-side taps/veths are torn down on stop like the primary NIC).
    #[serde(default)]
    pub extra_networks: Vec<NetworkConfig>,

    pub firmware: FirmwareConfig,
    pub audio: AudioConfig,
    pub input: InputConfig,

    /// Start this instance automatically when the daemon starts (§O). If the
    /// start fails the instance stays in Error/Stopped — never retried in a
    /// loop by the daemon.
    #[serde(default)]
    pub autostart: bool,
}

fn schema_version_default() -> u32 {
    1
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
        if self.gpu.render_backend != RenderBackend::Cpu {
            let gpu_mib = self.gpu.hostmem_bytes / GpuConfig::MIB;
            if !(256..=16384).contains(&gpu_mib) {
                return Err(format!(
                    "gpu.hostmem_bytes {gpu_mib} MiB is outside the sane range (256-16384 MiB)"
                ));
            }
        }
        if self.display.resolution.width == 0 || self.display.resolution.height == 0 {
            return Err(format!(
                "display resolution must be non-zero (got {}x{})",
                self.display.resolution.width, self.display.resolution.height
            ));
        }
        for disk in &self.extra_disks {
            if disk.size_bytes == 0 {
                return Err(format!(
                    "extra disk {} has size 0; attach it with a positive --size",
                    disk.path.display()
                ));
            }
        }
        self.network.validate()?;
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
    fn instance_id_is_lowercase_hex_of_len_64() {
        let id = InstanceId::new().to_string();
        assert_eq!(id.len(), INSTANCE_ID_HEX_LEN);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(id, id.to_lowercase());
    }

    #[test]
    fn instance_id_parses_from_hex_string() {
        let id = InstanceId::new();
        let hex = id.to_string();
        let parsed: InstanceId = hex.parse().expect("valid hex ID");
        assert_eq!(parsed, id);
        let upper: InstanceId = hex.to_uppercase().parse().expect("uppercase hex accepted");
        assert_eq!(upper, id);
        assert!("abc".parse::<InstanceId>().is_err());
        assert!("zzzz".repeat(16).parse::<InstanceId>().is_err());
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
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
            autostart: false,
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
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
            autostart: false,
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

    #[test]
    fn validate_rejects_out_of_range_gpu_memory_for_hardware_backend() {
        let mut cfg = sample_valid_config();
        cfg.gpu.render_backend = RenderBackend::Venus;
        cfg.gpu.hostmem_bytes = 64 * GpuConfig::MIB;
        assert!(cfg.validate().is_err());

        let mut cfg2 = sample_valid_config();
        cfg2.gpu.render_backend = RenderBackend::Venus;
        cfg2.gpu.hostmem_bytes = 32768 * GpuConfig::MIB;
        assert!(cfg2.validate().is_err());
    }

    #[test]
    fn validate_allows_small_hostmem_for_cpu_backend() {
        let mut cfg = sample_valid_config();
        cfg.gpu.render_backend = RenderBackend::Cpu;
        cfg.gpu.hostmem_bytes = 64 * GpuConfig::MIB;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn missing_schema_version_reads_back_as_v1() {
        let cfg = sample_valid_config();
        let mut value = serde_json::to_value(&cfg).expect("serialize");
        value
            .as_object_mut()
            .expect("object")
            .remove("schema_version");
        let restored: InstanceConfig = serde_json::from_value(value).expect("deserialize");
        assert_eq!(restored.schema_version, 1);
    }

    #[test]
    fn migrate_schema_is_noop_at_current_version() {
        let mut cfg = sample_valid_config();
        cfg.schema_version = 1;
        crate::config::migrate_schema(&mut cfg).expect("current version migrates cleanly");
        assert_eq!(cfg.schema_version, crate::config::CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn migrate_schema_rejects_unknown_future_version() {
        let mut cfg = sample_valid_config();
        cfg.schema_version = crate::config::CURRENT_SCHEMA_VERSION + 1;
        assert!(crate::config::migrate_schema(&mut cfg).is_err());
    }

    #[test]
    fn migrate_schema_rejects_version_zero() {
        let mut cfg = sample_valid_config();
        cfg.schema_version = 0;
        assert!(crate::config::migrate_schema(&mut cfg).is_err());
    }
}
