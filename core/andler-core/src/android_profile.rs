use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{
    AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
    InputConfig, InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AndroidVersion {
    Android11,
    Android13,
}

impl std::fmt::Display for AndroidVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AndroidVersion::Android11 => write!(f, "11"),
            AndroidVersion::Android13 => write!(f, "13"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArmTranslator {
    #[default]
    None,

    Libndk,

    Libhoudini,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AndroidBootMode {
    #[default]
    Android,
    Linux,
}

impl std::fmt::Display for AndroidBootMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AndroidBootMode::Android => write!(f, "android"),
            AndroidBootMode::Linux => write!(f, "linux"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseImagePin {
    /// Stable id of the base image the instance was created from
    /// (`android{version}-{variant}-{built_at}` from its manifest).
    pub id: String,
    /// sha256 of the base image qcow2 at creation time, as hex.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndroidProfile {
    pub android_version: AndroidVersion,
    pub gapps: bool,
    pub microg: bool,

    pub arm_translator: ArmTranslator,

    #[serde(default)]
    pub boot_mode: AndroidBootMode,

    /// Base-image pin written at instance creation. When the instance file
    /// already carries one, creation refuses a base image whose id or
    /// checksum no longer matches — a silently swapped backing image
    /// is a data-corruption trap, not an upgrade path.
    #[serde(default)]
    pub base_image_pin: Option<BaseImagePin>,
}

impl AndroidProfile {
    pub fn cache_key(&self) -> String {
        format!(
            "{:?}-gapps_{}-microg_{}-arm_{:?}",
            self.android_version, self.gapps, self.microg, self.arm_translator
        )
    }

    pub fn resolve(
        &self,
        instance_name: String,
        disk: DiskConfig,
        ovmf_vars_path: PathBuf,
    ) -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: instance_name,
            kind: InstanceKind::AndroidVm {
                android_profile: self.clone(),
            },
            backend: BackendKind::Qemu,
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk,
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(ovmf_vars_path),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
            autostart: false,
        }
    }
}

impl std::str::FromStr for ArmTranslator {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" | "no" | "" => Ok(ArmTranslator::None),
            "libndk" | "ndk" => Ok(ArmTranslator::Libndk),
            "libhoudini" | "houdini" => Ok(ArmTranslator::Libhoudini),
            _ => Err(format!(
                "unknown ARM translator: {s:?} (expected: none, libndk, or libhoudini)"
            )),
        }
    }
}

impl std::fmt::Display for ArmTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArmTranslator::None => write!(f, "none"),
            ArmTranslator::Libndk => write!(f, "libndk"),
            ArmTranslator::Libhoudini => write!(f, "libhoudini"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> AndroidProfile {
        AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            arm_translator: ArmTranslator::Libndk,
            boot_mode: AndroidBootMode::Android,
            base_image_pin: None,
        }
    }

    #[test]
    fn cache_key_differs_on_arm_translator() {
        let mut a = sample_profile();
        let mut b = sample_profile();
        a.arm_translator = ArmTranslator::Libndk;
        b.arm_translator = ArmTranslator::Libhoudini;
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn cache_key_differs_on_gapps() {
        let mut a = sample_profile();
        let mut b = sample_profile();
        a.gapps = true;
        b.gapps = false;
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn resolve_produces_overlay_disk_pointing_at_base_image() {
        let profile = sample_profile();
        let base = PathBuf::from("/var/lib/andler/images/android13-gapps-libndk.qcow2");
        let overlay = PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2");
        let disk = DiskConfig::overlay(overlay.clone(), base.clone(), 20 * DiskConfig::GIB);

        let cfg = profile.resolve(
            "my-android".to_string(),
            disk,
            PathBuf::from("/var/lib/andler/instances/abc/VARS.fd"),
        );

        assert_eq!(cfg.name, "my-android");
        assert_eq!(cfg.disk.base_image, Some(base));
        assert_eq!(cfg.disk.path, overlay);
        match cfg.kind {
            InstanceKind::AndroidVm { android_profile } => {
                assert_eq!(android_profile, profile);
            }
            InstanceKind::LinuxVm { .. } => panic!("expected AndroidVm"),
        }
    }

    #[test]
    fn resolve_accepts_standalone_disk_with_no_base_image() {
        let profile = sample_profile();
        let disk_path = PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2");
        let disk = DiskConfig::standalone(disk_path.clone(), 20 * DiskConfig::GIB);

        let cfg = profile.resolve(
            "my-android".to_string(),
            disk,
            PathBuf::from("/var/lib/andler/instances/abc/VARS.fd"),
        );

        assert_eq!(cfg.disk.base_image, None);
        assert_eq!(cfg.disk.path, disk_path);
    }
}
