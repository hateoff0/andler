use crate::android_profile::ArmTranslator;
use crate::config::{InstanceConfig, InstanceKind};

/// A guest-side action implied by an instance's own configuration. The
/// wizard asks for these once; this is where the answers turn back into
/// something applicable to a disk, so the CLI, the daemon, and any future
/// front end derive the same list from the same `instance.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSelection {
    /// Stable key for reporting (`arm-translator`, `spice-vdagent`).
    pub name: &'static str,
    pub kind: GuestSelectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestSelectionKind {
    /// Install (or replace) the ARM translation layer on an Android instance.
    ArmTranslator(ArmTranslator),
    /// Install a package inside the guest with its own package manager.
    Package(&'static str),
}

impl GuestSelection {
    /// Human-readable one-liner for CLI output.
    pub fn describe(&self) -> String {
        match &self.kind {
            GuestSelectionKind::ArmTranslator(translator) => {
                format!("ARM translator ({translator})")
            }
            GuestSelectionKind::Package(package) => {
                format!("guest package `{package}`")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestSelectionStatus {
    Applied,
    AlreadyPresent,
    Skipped,
    Failed,
}

impl GuestSelectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GuestSelectionStatus::Applied => "applied",
            GuestSelectionStatus::AlreadyPresent => "already_present",
            GuestSelectionStatus::Skipped => "skipped",
            GuestSelectionStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSelectionOutcome {
    pub name: &'static str,
    pub status: GuestSelectionStatus,
    pub message: String,
}

impl GuestSelectionOutcome {
    pub fn new(name: &'static str, status: GuestSelectionStatus, message: String) -> Self {
        Self {
            name,
            status,
            message,
        }
    }

    /// `andler guest apply <id>` and the wizard both print this; keep the
    /// retry command in the failure text so a partial apply is recoverable
    /// without re-reading the docs.
    pub fn failure(
        name: &'static str,
        instance_id: &str,
        selection: &GuestSelection,
        message: String,
    ) -> Self {
        let retry = match &selection.kind {
            GuestSelectionKind::ArmTranslator(translator) => {
                format!("andler guest install {translator} {instance_id}")
            }
            GuestSelectionKind::Package(package) => {
                format!("andler guest install {package} {instance_id}")
            }
        };
        Self::new(
            name,
            GuestSelectionStatus::Failed,
            format!("{message} — retry with `{retry}`"),
        )
    }
}

/// The guest-side work this instance's configuration asks for:
/// the ARM translator for an Android instance that selected one, and the
/// SPICE clipboard agent whenever clipboard sharing is enabled (the host
/// side of clipboard sharing is wired by QEMU; without the guest agent the
/// setting silently does nothing).
///
/// Order matters only for reporting; the caller applies each entry in turn.
pub fn guest_selections(cfg: &InstanceConfig) -> Vec<GuestSelection> {
    let mut selections = Vec::new();

    if let InstanceKind::AndroidVm { android_profile } = &cfg.kind {
        if android_profile.arm_translator != ArmTranslator::None {
            selections.push(GuestSelection {
                name: "arm-translator",
                kind: GuestSelectionKind::ArmTranslator(android_profile.arm_translator),
            });
        }
    }

    if cfg.input.clipboard_enabled {
        selections.push(GuestSelection {
            name: "spice-vdagent",
            kind: GuestSelectionKind::Package("spice-vdagent"),
        });
    }

    selections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_profile::{AndroidBootMode, AndroidVersion};
    use crate::config::{
        BackendKind, CdromBus, CpuConfig, DisplayConfig, FirmwareConfig, GpuConfig, InputConfig,
        MemoryConfig, NetworkConfig, CURRENT_SCHEMA_VERSION,
    };
    use std::path::PathBuf;

    fn cfg(kind: InstanceKind, clipboard_enabled: bool) -> InstanceConfig {
        InstanceConfig {
            id: crate::config::InstanceId::new(),
            name: "test-vm".to_string(),
            kind,
            backend: BackendKind::Qemu,
            schema_version: CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: crate::config::DiskConfig::reference_default(PathBuf::from(
                "/tmp/test-disk.qcow2",
            )),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/VARS.fd")),
            audio: crate::config::AudioConfig::reference_default(),
            input: InputConfig {
                clipboard_enabled,
                ..InputConfig::reference_default()
            },
            autostart: false,
        }
    }

    fn linux() -> InstanceKind {
        InstanceKind::LinuxVm {
            iso_path: PathBuf::from("/tmp/test.iso"),
            cdrom_bus: CdromBus::Ide,
        }
    }

    fn android(translator: ArmTranslator) -> InstanceKind {
        InstanceKind::AndroidVm {
            android_profile: crate::AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: false,
                microg: false,
                arm_translator: translator,
                boot_mode: AndroidBootMode::Android,
                base_image_pin: None,
            },
        }
    }

    #[test]
    fn android_with_a_translator_and_clipboard_asks_for_both() {
        let selections = guest_selections(&cfg(android(ArmTranslator::Libndk), true));

        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].name, "arm-translator");
        assert_eq!(
            selections[0].kind,
            GuestSelectionKind::ArmTranslator(ArmTranslator::Libndk)
        );
        assert_eq!(selections[1].name, "spice-vdagent");
        assert_eq!(
            selections[1].kind,
            GuestSelectionKind::Package("spice-vdagent")
        );
        assert!(selections[0].describe().contains("libndk"));
    }

    #[test]
    fn android_without_a_translator_only_asks_for_clipboard() {
        let selections = guest_selections(&cfg(android(ArmTranslator::None), true));

        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].name, "spice-vdagent");
    }

    #[test]
    fn clipboard_disabled_drops_the_agent_selection() {
        let selections = guest_selections(&cfg(linux(), false));

        assert!(selections.is_empty(), "nothing to apply: {selections:?}");
    }

    #[test]
    fn linux_with_clipboard_asks_only_for_the_agent() {
        let selections = guest_selections(&cfg(linux(), true));

        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].name, "spice-vdagent");
    }

    #[test]
    fn disk_path_does_not_influence_the_selection_list() {
        let mut cfg = cfg(android(ArmTranslator::Libhoudini), false);
        cfg.disk =
            crate::config::DiskConfig::reference_default(PathBuf::from("/tmp/other-disk.qcow2"));

        let selections = guest_selections(&cfg);

        assert_eq!(selections.len(), 1);
        assert_eq!(
            selections[0].kind,
            GuestSelectionKind::ArmTranslator(ArmTranslator::Libhoudini)
        );
    }

    #[test]
    fn failure_outcome_carries_a_retry_command_for_the_selection() {
        let selection = GuestSelection {
            name: "spice-vdagent",
            kind: GuestSelectionKind::Package("spice-vdagent"),
        };

        let outcome = GuestSelectionOutcome::failure(
            selection.name,
            "abcdef01",
            &selection,
            "guestmount failed".to_string(),
        );

        assert_eq!(outcome.status, GuestSelectionStatus::Failed);
        assert!(outcome
            .message
            .contains("andler guest install spice-vdagent abcdef01"));
        assert_eq!(GuestSelectionStatus::Applied.as_str(), "applied");
    }
}
