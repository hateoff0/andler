use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskFormat {
    Qcow2,
    Raw,
    Vdi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskConfig {
    pub path: PathBuf,

    pub size_bytes: u64,
    pub format: DiskFormat,

    pub base_image: Option<PathBuf>,

    pub thin_provisioning: bool,

    pub trim_on_shutdown: bool,

    pub compact_on_shutdown: bool,

    pub snapshot_timeout_secs: Option<u64>,
}

impl DiskConfig {
    pub const GIB: u64 = 1024 * 1024 * 1024;

    pub fn reference_default(path: PathBuf) -> Self {
        DiskConfig {
            path,
            size_bytes: 256 * Self::GIB,
            format: DiskFormat::Qcow2,
            base_image: None,
            thin_provisioning: true,
            trim_on_shutdown: true,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        }
    }

    pub fn overlay(path: PathBuf, base_image: PathBuf, size_bytes: u64) -> Self {
        DiskConfig {
            path,
            size_bytes,
            format: DiskFormat::Qcow2,
            base_image: Some(base_image),
            thin_provisioning: true,
            trim_on_shutdown: true,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        }
    }

    pub fn standalone(path: PathBuf, size_bytes: u64) -> Self {
        DiskConfig {
            path,
            size_bytes,
            format: DiskFormat::Qcow2,
            base_image: None,
            thin_provisioning: true,
            trim_on_shutdown: true,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_is_256_gib_thin_provisioned_qcow2() {
        let cfg = DiskConfig::reference_default(PathBuf::from("disk.qcow2"));
        assert_eq!(cfg.size_bytes, 256 * DiskConfig::GIB);
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert_eq!(cfg.base_image, None);
        assert!(cfg.thin_provisioning);
        assert!(cfg.trim_on_shutdown);
        assert!(
            !cfg.compact_on_shutdown,
            "compact_on_shutdown must be opt-in, not a default-on behavior"
        );
    }

    #[test]
    fn standalone_has_no_base_image() {
        let cfg = DiskConfig::standalone(
            PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2"),
            20 * DiskConfig::GIB,
        );
        assert_eq!(cfg.base_image, None);
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert_eq!(cfg.size_bytes, 20 * DiskConfig::GIB);
        assert!(!cfg.compact_on_shutdown);
    }

    #[test]
    fn overlay_points_at_base_image() {
        let base = PathBuf::from("/var/lib/andler/images/android-13-gapps.qcow2");
        let cfg = DiskConfig::overlay(
            PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2"),
            base.clone(),
            20 * DiskConfig::GIB,
        );
        assert_eq!(cfg.base_image, Some(base));
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert!(!cfg.compact_on_shutdown);
    }
}
