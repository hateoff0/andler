use std::path::PathBuf;

use super::{
    AudioConfig, BackendKind, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig,
    GpuConfig, InputConfig, InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
    CURRENT_SCHEMA_VERSION,
};
use crate::android_profile::AndroidProfile;

pub const DEFAULT_DISK_GIB: u64 = 256;

pub const DEFAULT_OVERLAY_GIB: u64 = 128;

/// The OVMF pair the host offers, as discovered by whoever builds the draft.
/// `andler-core` owns the resolution but not the discovery: the CLI probes the
/// distro layouts, a daemon-side caller passes its configured pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFirmware {
    pub ovmf_code_path: Option<PathBuf>,
    pub ovmf_vars_template: Option<PathBuf>,
}

/// What a draft asks for, with the identity fields the kind owns.
#[derive(Debug, Clone)]
pub enum DraftKind {
    Linux {
        iso_path: PathBuf,
        cdrom_bus: CdromBus,
    },
    Android {
        profile: AndroidProfile,
    },
}

/// Where the instance disk comes from. Both variants produce a standalone
/// qcow2 unless `linked` asks for a backing-file overlay on a base image.
#[derive(Debug, Clone)]
pub enum DiskSource {
    Fresh,
    BaseImage { path: PathBuf, linked: bool },
}

#[derive(Debug, Clone)]
pub struct DiskDraft {
    pub path: PathBuf,
    pub source: DiskSource,
    pub size_gib: Option<u64>,
    pub compact_on_shutdown: bool,
    pub snapshot_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct FirmwareDraft {
    pub enable_uefi: Option<bool>,
    pub ovmf_vars_template: Option<PathBuf>,
}

/// Everything a creation path may choose. Every `None` section means "the
/// reference default" and every `None` path means "whatever the host offers",
/// so CLI flags, an instance file and a wizard answer set each fill only the
/// fields they own and resolve to the same config for the same input.
#[derive(Debug, Clone)]
pub struct ConfigDraft {
    pub name: String,
    pub kind: DraftKind,
    pub disk: DiskDraft,
    pub firmware: FirmwareDraft,
    pub autostart: bool,
    pub cpu: Option<CpuConfig>,
    pub memory: Option<MemoryConfig>,
    pub display: Option<DisplayConfig>,
    pub gpu: Option<GpuConfig>,
    pub network: Option<NetworkConfig>,
    pub audio: Option<AudioConfig>,
    pub input: Option<InputConfig>,
}

impl ConfigDraft {
    /// Fills every unset section with its reference default, resolves the disk
    /// and the firmware, and produces the config creation intends to send.
    /// Validation is `InstanceConfig::validate` on the result — one entry
    /// point, so a size or range check added to a section covers every path.
    pub fn resolve(&self, host: &HostFirmware) -> Result<InstanceConfig, String> {
        let disk = self.disk.resolve()?;
        let firmware = self.firmware.resolve(host);

        let base = InstanceConfig {
            id: InstanceId::new(),
            name: self.name.clone(),
            kind: match &self.kind {
                DraftKind::Linux {
                    iso_path,
                    cdrom_bus,
                } => InstanceKind::LinuxVm {
                    iso_path: iso_path.clone(),
                    cdrom_bus: *cdrom_bus,
                },
                DraftKind::Android { profile } => InstanceKind::AndroidVm {
                    android_profile: profile.clone(),
                },
            },
            backend: BackendKind::Qemu,
            schema_version: CURRENT_SCHEMA_VERSION,
            cpu: self
                .cpu
                .clone()
                .unwrap_or_else(CpuConfig::reference_default),
            memory: self
                .memory
                .clone()
                .unwrap_or_else(MemoryConfig::reference_default),
            disk,
            display: self
                .display
                .unwrap_or_else(DisplayConfig::reference_default),
            gpu: self
                .gpu
                .clone()
                .unwrap_or_else(GpuConfig::reference_default),
            network: self
                .network
                .clone()
                .unwrap_or_else(NetworkConfig::reference_default),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware,
            audio: self.audio.unwrap_or_else(AudioConfig::reference_default),
            input: self.input.unwrap_or_else(InputConfig::reference_default),
            autostart: self.autostart,
        };

        if let InstanceKind::AndroidVm { .. } = &base.kind {
            self.reject_unsupported_android_inputs()?;
            return Ok(InstanceConfig {
                autostart: false,
                ..base
            });
        }

        Ok(base)
    }

    /// Android instances take their sections from the `(android_profile)`
    /// defaults — `CreateAndroidInstanceRequest` carries no config sections —
    /// so an explicitly supplied section would be dropped silently. Refuse it
    /// with the same rule `andler create --template` applies for Android.
    fn reject_unsupported_android_inputs(&self) -> Result<(), String> {
        let section = if self.cpu.is_some() {
            Some("cpu")
        } else if self.memory.is_some() {
            Some("memory")
        } else if self.display.is_some() {
            Some("display")
        } else if self.gpu.is_some() {
            Some("gpu")
        } else if self.network.is_some() {
            Some("network")
        } else if self.audio.is_some() {
            Some("audio")
        } else if self.input.is_some() {
            Some("input")
        } else {
            None
        };
        if let Some(section) = section {
            return Err(format!(
                "the `{section}` section is not supported for Android VMs in this phase \
                 (Android requests carry no config sections)"
            ));
        }
        if self.autostart {
            return Err(
                "`autostart` is not supported for Android VMs in this phase \
                 (Android requests carry no autostart field)"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl DiskDraft {
    fn resolve(&self) -> Result<DiskConfig, String> {
        let gib = self.size_gib.unwrap_or(match self.source {
            DiskSource::Fresh => DEFAULT_DISK_GIB,
            DiskSource::BaseImage { .. } => DEFAULT_OVERLAY_GIB,
        });
        let size_bytes = gib.checked_mul(DiskConfig::GIB).ok_or_else(|| {
            format!(
                "disk size {gib} GiB is too large (the largest supported size is {} GiB)",
                u64::MAX / DiskConfig::GIB
            )
        })?;

        let mut disk = match &self.source {
            DiskSource::Fresh => DiskConfig::standalone(self.path.clone(), size_bytes),
            DiskSource::BaseImage { path, linked: true } => {
                DiskConfig::overlay(self.path.clone(), path.clone(), size_bytes)
            }
            DiskSource::BaseImage {
                path: _,
                linked: false,
            } => DiskConfig::standalone(self.path.clone(), size_bytes),
        };
        disk.compact_on_shutdown = self.compact_on_shutdown;
        disk.snapshot_timeout_secs = self.snapshot_timeout_secs;
        Ok(disk)
    }
}

impl FirmwareDraft {
    fn resolve(&self, host: &HostFirmware) -> FirmwareConfig {
        let explicit = self
            .ovmf_vars_template
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty());
        let available = explicit.is_some() || host.ovmf_vars_template.is_some();
        let enable_uefi = self.enable_uefi.unwrap_or(available);

        if !enable_uefi {
            return FirmwareConfig {
                enable_uefi: false,
                ovmf_code_path: PathBuf::new(),
                ovmf_vars_path: PathBuf::new(),
            };
        }

        FirmwareConfig {
            enable_uefi: true,
            ovmf_code_path: PathBuf::new(),
            ovmf_vars_path: explicit.cloned().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_profile::{AndroidBootMode, AndroidVersion, ArmTranslator};
    use crate::config::DiskFormat;

    fn host() -> HostFirmware {
        HostFirmware {
            ovmf_code_path: Some(PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")),
            ovmf_vars_template: Some(PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        }
    }

    fn linux_draft() -> ConfigDraft {
        ConfigDraft {
            name: "resolver-vm".to_string(),
            kind: DraftKind::Linux {
                iso_path: PathBuf::from("/isos/ubuntu-24.04.iso"),
                cdrom_bus: CdromBus::VirtioScsi,
            },
            disk: DiskDraft {
                path: PathBuf::from("/instances/disk.qcow2"),
                source: DiskSource::Fresh,
                size_gib: None,
                compact_on_shutdown: false,
                snapshot_timeout_secs: None,
            },
            firmware: FirmwareDraft::default(),
            autostart: false,
            cpu: None,
            memory: None,
            display: None,
            gpu: None,
            network: None,
            audio: None,
            input: None,
        }
    }

    fn android_draft() -> ConfigDraft {
        ConfigDraft {
            name: "resolver-android".to_string(),
            kind: DraftKind::Android {
                profile: AndroidProfile {
                    android_version: AndroidVersion::Android13,
                    gapps: true,
                    microg: false,
                    arm_translator: ArmTranslator::Libndk,
                    boot_mode: AndroidBootMode::Android,
                    base_image_pin: None,
                },
            },
            disk: DiskDraft {
                path: PathBuf::from("/instances/disk.qcow2"),
                source: DiskSource::BaseImage {
                    path: PathBuf::from("/cache/android13-gapps/base.qcow2"),
                    linked: true,
                },
                size_gib: None,
                compact_on_shutdown: false,
                snapshot_timeout_secs: None,
            },
            firmware: FirmwareDraft::default(),
            autostart: false,
            cpu: None,
            memory: None,
            display: None,
            gpu: None,
            network: None,
            audio: None,
            input: None,
        }
    }

    #[test]
    fn unset_sections_resolve_to_the_reference_defaults() {
        let cfg = linux_draft().resolve(&host()).expect("resolves");
        assert_eq!(cfg.cpu, CpuConfig::reference_default());
        assert_eq!(cfg.memory, MemoryConfig::reference_default());
        assert_eq!(cfg.display, DisplayConfig::reference_default());
        assert_eq!(cfg.gpu, GpuConfig::reference_default());
        assert_eq!(cfg.network, NetworkConfig::reference_default());
        assert_eq!(cfg.audio, AudioConfig::reference_default());
        assert_eq!(cfg.input, InputConfig::reference_default());
        cfg.validate().expect("a defaulted config is valid");
    }

    #[test]
    fn unset_disk_size_resolves_to_the_documented_default() {
        let cfg = linux_draft().resolve(&host()).expect("resolves");
        assert_eq!(cfg.disk.size_bytes, DEFAULT_DISK_GIB * DiskConfig::GIB);
        assert_eq!(cfg.disk.format, DiskFormat::Qcow2);
        assert_eq!(cfg.disk.base_image, None);
    }

    #[test]
    fn android_base_image_disk_defaults_to_the_overlay_size() {
        let cfg = android_draft().resolve(&host()).expect("resolves");
        assert_eq!(cfg.disk.size_bytes, DEFAULT_OVERLAY_GIB * DiskConfig::GIB);
        assert_eq!(
            cfg.disk.base_image,
            Some(PathBuf::from("/cache/android13-gapps/base.qcow2"))
        );
    }

    #[test]
    fn android_standalone_disk_keeps_the_base_image_path_out_of_the_config() {
        let mut draft = android_draft();
        draft.disk.source = DiskSource::BaseImage {
            path: PathBuf::from("/cache/android13-gapps/base.qcow2"),
            linked: false,
        };
        let cfg = draft.resolve(&host()).expect("resolves");
        assert_eq!(cfg.disk.base_image, None);
        assert_eq!(cfg.disk.size_bytes, DEFAULT_OVERLAY_GIB * DiskConfig::GIB);
    }

    #[test]
    fn oversized_disk_is_reported_instead_of_panicking() {
        let mut draft = linux_draft();
        draft.disk.size_gib = Some(u64::MAX);
        let err = draft.resolve(&host()).expect_err("must be rejected");
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn firmware_defaults_to_uefi_when_the_host_has_a_template() {
        let cfg = linux_draft().resolve(&host()).expect("resolves");
        assert!(cfg.firmware.enable_uefi);
        assert!(
            cfg.firmware.ovmf_code_path.as_os_str().is_empty(),
            "the OVMF code path stays the daemon's to resolve (ANDLERD_OVMF_CODE)"
        );
        assert!(
            cfg.firmware.ovmf_vars_path.as_os_str().is_empty(),
            "with no explicit template the daemon provisions its own"
        );
    }

    #[test]
    fn firmware_defaults_to_legacy_bios_when_the_host_has_none() {
        let cfg = linux_draft()
            .resolve(&HostFirmware::default())
            .expect("resolves");
        assert!(!cfg.firmware.enable_uefi);
        assert!(cfg.firmware.ovmf_vars_path.as_os_str().is_empty());
    }

    #[test]
    fn explicit_ovmf_template_reaches_the_config_and_forces_uefi() {
        let mut draft = linux_draft();
        draft.firmware.ovmf_vars_template = Some(PathBuf::from("/fixtures/VARS.fd"));
        let cfg = draft
            .resolve(&HostFirmware::default())
            .expect("an explicit template is enough without host discovery");
        assert!(cfg.firmware.enable_uefi);
        assert_eq!(
            cfg.firmware.ovmf_vars_path,
            PathBuf::from("/fixtures/VARS.fd")
        );
    }

    #[test]
    fn explicit_no_uefi_drops_the_template() {
        let mut draft = linux_draft();
        draft.firmware.enable_uefi = Some(false);
        let cfg = draft.resolve(&host()).expect("resolves");
        assert!(!cfg.firmware.enable_uefi);
        assert!(cfg.firmware.ovmf_vars_path.as_os_str().is_empty());
    }

    #[test]
    fn android_sections_are_refused_rather_than_dropped() {
        let mut draft = android_draft();
        draft.cpu = Some(CpuConfig::reference_default());
        let err = draft.resolve(&host()).expect_err("must be refused");
        assert!(err.contains("`cpu` section"), "{err}");

        let mut draft = android_draft();
        draft.memory = Some(MemoryConfig::reference_default());
        let err = draft.resolve(&host()).expect_err("must be refused");
        assert!(err.contains("`memory` section"), "{err}");
    }

    #[test]
    fn android_autostart_is_refused_rather_than_dropped() {
        let mut draft = android_draft();
        draft.autostart = true;
        let err = draft.resolve(&host()).expect_err("must be refused");
        assert!(err.contains("autostart"), "{err}");
    }

    #[test]
    fn android_profile_is_carried_into_the_kind() {
        let cfg = android_draft().resolve(&host()).expect("resolves");
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.android_version, AndroidVersion::Android13);
        assert!(android_profile.gapps);
        assert_eq!(android_profile.arm_translator, ArmTranslator::Libndk);
        assert!(!cfg.autostart);
    }

    #[test]
    fn validation_outcome_is_the_resolver_output_not_a_second_rule() {
        let mut draft = linux_draft();
        draft.cpu = Some(CpuConfig {
            cores: 0,
            ..CpuConfig::reference_default()
        });
        let cfg = draft.resolve(&host()).expect("resolving is not validating");
        assert_eq!(
            cfg.validate().expect_err("zero cores must fail"),
            "cpu.cores must be at least 1"
        );
    }
}
