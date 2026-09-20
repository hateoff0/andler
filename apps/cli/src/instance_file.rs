use std::path::{Path, PathBuf};

use andler_core::config::{ConfigDraft, DiskDraft, DiskSource, DraftKind, FirmwareDraft};
use andler_core::{
    AndroidBootMode, AndroidProfile, AndroidVersion, ArmTranslator, AudioConfig, CdromBus,
    CpuConfig, DisplayConfig, GpuConfig, InputConfig, MemoryConfig, NetworkConfig,
};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum InstanceFileError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path} as TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("template {name:?} not found (no {user_path} and no built-in of that name); built-ins: headless, desktop")]
    TemplateNotFound { name: String, user_path: PathBuf },

    #[error("invalid {field} {path:?}: {source}")]
    InvalidPath {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("missing required field {field} in instance file")]
    MissingField { field: &'static str },

    #[error("unsupported value for {field}: {value}")]
    UnsupportedValue { field: &'static str, value: String },
}

/// An instance file resolved to the one draft shape every creation path uses,
/// plus the instances root the daemon places the Android instance directory in.
#[derive(Debug)]
pub struct InstanceFileDraft {
    pub draft: ConfigDraft,
    pub instances_root: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceFileCdromBus {
    #[default]
    Auto,
    Virtio,
    Ide,
}

#[derive(Debug, Deserialize)]
pub struct InstanceFile {
    pub name: String,

    #[serde(default)]
    pub iso_path: Option<PathBuf>,

    #[serde(default)]
    pub disk_path: Option<PathBuf>,

    #[serde(default)]
    pub disk_size_gib: Option<u64>,

    #[serde(default)]
    pub compact_on_shutdown: bool,

    #[serde(default)]
    pub cdrom_bus: InstanceFileCdromBus,

    /// OVMF_VARS template for this instance. Omitted, the host's discovered
    /// pair is used, exactly like omitting `--ovmf-vars-template`.
    #[serde(default)]
    pub ovmf_vars_path: Option<PathBuf>,

    /// Omitted, UEFI is used when the host offers an OVMF pair and Legacy
    /// BIOS is used when it does not.
    #[serde(default)]
    pub enable_uefi: Option<bool>,

    /// Start this instance automatically when the daemon starts.
    #[serde(default)]
    pub autostart: bool,

    #[serde(default)]
    pub android_version: Option<u32>,

    #[serde(default)]
    pub base_image_path: Option<String>,

    /// Explicit base-image pin for Android instances. When set, creation
    /// refuses a backing image whose manifest id or sha256 does not match
    /// (see docs/API.md "Base-image pin"); omit to let the daemon record
    /// the pin of whatever image is resolved.
    #[serde(default)]
    pub base_image_pin: Option<andler_core::BaseImagePin>,

    #[serde(default)]
    pub overlay_size_gib: Option<u64>,

    #[serde(default)]
    pub linked_overlay: bool,

    #[serde(default)]
    pub gapps: bool,

    #[serde(default)]
    pub microg: bool,

    #[serde(default)]
    pub libndk: bool,

    #[serde(default)]
    pub arm_translator: Option<String>,

    #[serde(default)]
    pub instances_root: Option<String>,

    #[serde(default)]
    pub cpu: Option<CpuConfig>,
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
    #[serde(default)]
    pub display: Option<DisplayConfig>,
    #[serde(default)]
    pub gpu: Option<GpuConfig>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub audio: Option<AudioConfig>,
    #[serde(default)]
    pub input: Option<InputConfig>,

    #[serde(default)]
    pub snapshot_timeout_secs: Option<u64>,
}

/// A VM template: partial `InstanceConfig` sections without the identity
/// fields (name/kind/iso/disk come from the CLI flags) and without flag-
/// backed scalars (merge order is defaults < template < CLI flags, see
/// `andler create --template`).
#[derive(Debug, Default, serde::Deserialize)]
pub struct TemplateFile {
    #[serde(default)]
    pub cpu: Option<CpuConfig>,
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
    #[serde(default)]
    pub display: Option<DisplayConfig>,
    #[serde(default)]
    pub gpu: Option<GpuConfig>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub audio: Option<AudioConfig>,
    #[serde(default)]
    pub input: Option<InputConfig>,
}

impl TemplateFile {
    /// Loads a template by name: `~/.andler/templates/<name>.toml` when the
    /// file exists, otherwise one of the built-ins (`headless`, `desktop`).
    pub fn load(name: &str) -> Result<Self, InstanceFileError> {
        let user_path = andler_core::paths::andler_home()
            .join("templates")
            .join(format!("{name}.toml"));
        if user_path.exists() {
            let text =
                std::fs::read_to_string(&user_path).map_err(|source| InstanceFileError::Read {
                    path: user_path.clone(),
                    source,
                })?;
            return toml::from_str(&text).map_err(|source| InstanceFileError::Parse {
                path: user_path,
                source,
            });
        }

        let builtin = match name {
            "headless" => Some(include_str!("templates/headless.toml")),
            "desktop" => Some(include_str!("templates/desktop.toml")),
            _ => None,
        };
        match builtin {
            Some(text) => toml::from_str(text).map_err(|source| InstanceFileError::Parse {
                path: PathBuf::from(format!("builtin template {name:?}")),
                source,
            }),
            None => Err(InstanceFileError::TemplateNotFound {
                name: name.to_string(),
                user_path,
            }),
        }
    }
}

impl InstanceFile {
    pub fn load(path: &Path) -> Result<Self, InstanceFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| InstanceFileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut parsed: Self =
            toml::from_str(&text).map_err(|source| InstanceFileError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        parsed.canonicalize_paths()?;
        Ok(parsed)
    }

    fn canonicalize_paths(&mut self) -> Result<(), InstanceFileError> {
        if let Some(iso_path) = &self.iso_path {
            self.iso_path = Some(std::fs::canonicalize(iso_path).map_err(|source| {
                InstanceFileError::InvalidPath {
                    field: "iso_path",
                    path: iso_path.clone(),
                    source,
                }
            })?);
        }

        if let Some(disk_path) = &self.disk_path {
            if let Some(parent) = disk_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                let canonical_parent = std::fs::canonicalize(parent).map_err(|source| {
                    InstanceFileError::InvalidPath {
                        field: "disk_path",
                        path: disk_path.clone(),
                        source,
                    }
                })?;
                if let Some(file_name) = disk_path.file_name() {
                    self.disk_path = Some(canonical_parent.join(file_name));
                }
            }
        }

        if let Some(base_image_path) = &self.base_image_path {
            let canonical = std::fs::canonicalize(base_image_path).map_err(|source| {
                InstanceFileError::InvalidPath {
                    field: "base_image_path",
                    path: PathBuf::from(base_image_path),
                    source,
                }
            })?;
            self.base_image_path = Some(canonical.to_string_lossy().into_owned());
        }

        if let Some(ovmf_vars_path) = &self.ovmf_vars_path {
            self.ovmf_vars_path =
                Some(std::fs::canonicalize(ovmf_vars_path).map_err(|source| {
                    InstanceFileError::InvalidPath {
                        field: "ovmf_vars_path",
                        path: ovmf_vars_path.clone(),
                        source,
                    }
                })?);
        }

        Ok(())
    }

    /// Builds the draft the one resolver turns into the instance config.
    /// Paths were canonicalized by `load`; everything else is left to the
    /// resolver, so an omitted field means the same thing here as it does on
    /// the CLI-flag and wizard paths.
    pub fn into_draft(self) -> Result<InstanceFileDraft, InstanceFileError> {
        let instances_root = self
            .instances_root
            .clone()
            .unwrap_or_else(crate::create::default_instances_root);
        let android = self.android_version.is_some() || self.base_image_path.is_some();
        let firmware = FirmwareDraft {
            enable_uefi: self.enable_uefi,
            ovmf_vars_template: self.ovmf_vars_path.clone(),
        };
        let name = self.name.clone();

        let draft = if android {
            self.into_android_draft(&name, &instances_root, firmware)?
        } else {
            self.into_linux_draft(name, firmware)?
        };

        Ok(InstanceFileDraft {
            draft,
            instances_root,
        })
    }

    fn into_linux_draft(
        self,
        name: String,
        firmware: FirmwareDraft,
    ) -> Result<ConfigDraft, InstanceFileError> {
        let disk_path = self
            .disk_path
            .clone()
            .ok_or(InstanceFileError::MissingField { field: "disk_path" })?;
        let iso_path = self
            .iso_path
            .clone()
            .ok_or(InstanceFileError::MissingField { field: "iso_path" })?;
        let cdrom_bus = match self.cdrom_bus {
            InstanceFileCdromBus::Auto => CdromBus::recommended_for_iso_filename(&iso_path),
            InstanceFileCdromBus::Virtio => CdromBus::VirtioScsi,
            InstanceFileCdromBus::Ide => CdromBus::Ide,
        };

        Ok(ConfigDraft {
            name,
            kind: DraftKind::Linux {
                iso_path,
                cdrom_bus,
            },
            disk: DiskDraft {
                path: disk_path,
                source: DiskSource::Fresh,
                size_gib: self.disk_size_gib,
                compact_on_shutdown: self.compact_on_shutdown,
                snapshot_timeout_secs: self.snapshot_timeout_secs,
            },
            firmware,
            autostart: self.autostart,
            cpu: self.cpu,
            memory: self.memory,
            display: self.display,
            gpu: self.gpu,
            network: self.network,
            audio: self.audio,
            input: self.input,
        })
    }

    fn into_android_draft(
        self,
        name: &str,
        instances_root: &str,
        firmware: FirmwareDraft,
    ) -> Result<ConfigDraft, InstanceFileError> {
        let base_image_path = self.base_image_path.clone().unwrap_or_default();
        let android_version = match self.android_version.unwrap_or(13) {
            11 => AndroidVersion::Android11,
            13 => AndroidVersion::Android13,
            other => {
                return Err(InstanceFileError::UnsupportedValue {
                    field: "android_version",
                    value: other.to_string(),
                })
            }
        };

        let arm_translator = match self.arm_translator.as_deref() {
            Some(raw) => raw.parse::<ArmTranslator>().map_err(|reason| {
                InstanceFileError::UnsupportedValue {
                    field: "arm_translator",
                    value: format!("{raw} ({reason})"),
                }
            })?,
            None if self.libndk => ArmTranslator::Libndk,
            None => ArmTranslator::None,
        };

        let profile = AndroidProfile {
            android_version,
            gapps: self.gapps,
            microg: self.microg,
            arm_translator,
            boot_mode: AndroidBootMode::Android,
            base_image_pin: self.base_image_pin,
        };

        Ok(ConfigDraft {
            name: name.to_string(),
            kind: DraftKind::Android { profile },
            disk: DiskDraft {
                path: PathBuf::from(instances_root).join("disk.qcow2"),
                source: DiskSource::BaseImage {
                    path: PathBuf::from(base_image_path),
                    linked: self.linked_overlay,
                },
                size_gib: self.overlay_size_gib,
                compact_on_shutdown: false,
                snapshot_timeout_secs: None,
            },
            firmware,
            autostart: self.autostart,
            cpu: self.cpu,
            memory: self.memory,
            display: self.display,
            gpu: self.gpu,
            network: self.network,
            audio: self.audio,
            input: self.input,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::Creation;
    use andler_core::config::HostFirmware;
    use andler_core::{InstanceConfig, InstanceKind};

    fn host() -> HostFirmware {
        HostFirmware {
            ovmf_code_path: Some(PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")),
            ovmf_vars_template: Some(PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        }
    }

    fn draft(toml_text: &str) -> InstanceFileDraft {
        let file: InstanceFile = toml::from_str(toml_text).expect("TOML must parse");
        file.into_draft().expect("draft must build")
    }

    fn resolve(toml_text: &str) -> InstanceConfig {
        let file = draft(toml_text);
        Creation::resolve(&file.draft, file.instances_root, &host())
            .expect("must resolve")
            .cfg
    }

    fn resolve_err(toml_text: &str) -> String {
        let file = draft(toml_text);
        Creation::resolve(&file.draft, file.instances_root, &host()).expect_err("must be refused")
    }

    #[test]
    fn builtin_headless_template_parses() {
        let template = TemplateFile::load("headless").expect("built-in must parse");
        let gpu = template.gpu.expect("headless must set gpu");
        assert_eq!(gpu.render_backend, andler_core::RenderBackend::Cpu);
        let display = template.display.expect("headless must set display");
        assert_eq!(display.display_engine, andler_core::DisplayEngine::None);
        let audio = template.audio.expect("headless must set audio");
        assert_eq!(audio.backend, andler_core::AudioBackend::None);
    }

    #[test]
    fn builtin_desktop_template_parses() {
        let template = TemplateFile::load("desktop").expect("built-in must parse");
        let gpu = template.gpu.expect("desktop must set gpu");
        assert_eq!(gpu.render_backend, andler_core::RenderBackend::Venus);
        let display = template.display.expect("desktop must set display");
        assert_eq!(display.display_engine, andler_core::DisplayEngine::Sdl);
        assert_eq!(display.resolution, andler_core::Resolution::new(1920, 1080));
    }

    #[test]
    fn unknown_template_is_rejected_with_list_of_builtins() {
        let err = TemplateFile::load("definitely-not-a-template").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("not found"), "{text}");
        assert!(text.contains("headless"), "{text}");
    }

    const MINIMAL_LINUX_TOML: &str = r#"
        name = "test-vm"
        iso_path = "/tmp/test.iso"
        disk_path = "/tmp/disk.qcow2"
        ovmf_vars_path = "/tmp/test_VARS.fd"
    "#;

    const MINIMAL_ANDROID_TOML: &str = r#"
        name = "test-android"
        android_version = 13
        base_image_path = "/tmp/base.qcow2"
        ovmf_vars_path = "/tmp/test_VARS.fd"
    "#;

    #[test]
    fn minimal_linux_file_parses_and_fills_every_section() {
        let cfg = resolve(MINIMAL_LINUX_TOML);
        assert_eq!(cfg.name, "test-vm");
        assert_eq!(
            cfg.kind,
            InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::Ide,
            }
        );
        assert_eq!(cfg.cpu, CpuConfig::reference_default());
        assert_eq!(cfg.memory, MemoryConfig::reference_default());
        assert_eq!(cfg.display, DisplayConfig::reference_default());
        assert_eq!(cfg.gpu, GpuConfig::reference_default());
        assert_eq!(cfg.network, NetworkConfig::reference_default());
        assert_eq!(cfg.audio, AudioConfig::reference_default());
        assert_eq!(cfg.input, InputConfig::reference_default());
        cfg.validate()
            .expect("a minimal file resolves to a valid config");
    }

    #[test]
    fn an_omitted_ovmf_template_resolves_like_an_omitted_flag() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
            disk_path = "/tmp/disk.qcow2"
        "#;
        let cfg = resolve(toml);
        assert!(cfg.firmware.enable_uefi);
        assert!(
            cfg.firmware.ovmf_vars_path.as_os_str().is_empty(),
            "an omitted template means the daemon provisions its own, exactly like omitting \
             --ovmf-vars-template"
        );
    }

    #[test]
    fn an_omitted_uefi_choice_follows_the_host_firmware() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
            disk_path = "/tmp/disk.qcow2"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        let file_draft = file.into_draft().expect("draft");
        let with_firmware = Creation::resolve(
            &file_draft.draft,
            file_draft.instances_root.clone(),
            &host(),
        )
        .expect("resolves");
        assert!(with_firmware.cfg.firmware.enable_uefi);

        let without_firmware = Creation::resolve(
            &file_draft.draft,
            file_draft.instances_root,
            &HostFirmware::default(),
        )
        .expect("resolves");
        assert!(
            !without_firmware.cfg.firmware.enable_uefi,
            "without a host OVMF pair UEFI is off, never an empty pflash path"
        );
    }

    #[test]
    fn linux_disk_size_gib_overrides_default() {
        let toml = format!("{MINIMAL_LINUX_TOML}\ndisk_size_gib = 100\n");
        let cfg = resolve(&toml);
        assert_eq!(cfg.disk.size_bytes, 100 * 1024 * 1024 * 1024);
    }

    #[test]
    fn linux_compact_on_shutdown_defaults_to_false() {
        let cfg = resolve(MINIMAL_LINUX_TOML);
        assert!(
            !cfg.disk.compact_on_shutdown,
            "compact_on_shutdown must default to false when absent from TOML"
        );
    }

    #[test]
    fn linux_compact_on_shutdown_can_be_enabled() {
        let toml = format!("{MINIMAL_LINUX_TOML}\ncompact_on_shutdown = true\n");
        let cfg = resolve(&toml);
        assert!(cfg.disk.compact_on_shutdown);
    }

    #[test]
    fn linux_cdrom_bus_defaults_to_auto_detect_by_iso_filename() {
        let cfg = resolve(MINIMAL_LINUX_TOML);
        let InstanceKind::LinuxVm { cdrom_bus, .. } = cfg.kind else {
            panic!("expected a Linux VM");
        };
        assert_eq!(cdrom_bus, CdromBus::Ide);
    }

    #[test]
    fn linux_cdrom_bus_auto_detects_virtio_for_known_distro_filename() {
        let toml = MINIMAL_LINUX_TOML.replace("/tmp/test.iso", "/tmp/ubuntu-24.04.iso");
        let cfg = resolve(&toml);
        let InstanceKind::LinuxVm { cdrom_bus, .. } = cfg.kind else {
            panic!("expected a Linux VM");
        };
        assert_eq!(cdrom_bus, CdromBus::VirtioScsi);
    }

    #[test]
    fn linux_cdrom_bus_explicit_choice_overrides_auto_detect() {
        let toml = MINIMAL_LINUX_TOML.replace("/tmp/test.iso", "/tmp/ubuntu-24.04.iso")
            + "\ncdrom_bus = \"ide\"\n";
        let cfg = resolve(&toml);
        let InstanceKind::LinuxVm { cdrom_bus, .. } = cfg.kind else {
            panic!("expected a Linux VM");
        };
        assert_eq!(cdrom_bus, CdromBus::Ide);
    }

    #[test]
    fn minimal_android_file_parses() {
        let cfg = resolve(MINIMAL_ANDROID_TOML);
        assert_eq!(cfg.name, "test-android");
        assert_eq!(
            cfg.disk.size_bytes,
            128 * 1024 * 1024 * 1024,
            "overlay default must be 128 GiB when overlay_size_gib is omitted"
        );
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.android_version, AndroidVersion::Android13);
    }

    #[test]
    fn android_with_all_options() {
        let toml = format!(
            "{MINIMAL_ANDROID_TOML}\n\
             overlay_size_gib = 30\n\
             gapps = true\n\
             microg = true\n\
             libndk = true\n"
        );
        let cfg = resolve(&toml);
        assert_eq!(cfg.disk.size_bytes, 30 * 1024 * 1024 * 1024);
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert!(android_profile.gapps);
        assert!(android_profile.microg);
        assert_eq!(android_profile.arm_translator, ArmTranslator::Libndk);
    }

    #[test]
    fn android_base_image_pin_parses_and_reaches_the_request() {
        let toml = format!(
            "{MINIMAL_ANDROID_TOML}\n\
             base_image_pin = {{ id = \"android13-vanilla-2099-01-01T00:00:00Z\", \
             sha256 = \"{}\" }}\n",
            "ab".repeat(32)
        );
        let file = draft(&toml);
        let creation =
            Creation::resolve(&file.draft, file.instances_root.clone(), &host()).expect("resolves");
        let InstanceKind::AndroidVm { android_profile } = &creation.cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(
            android_profile.base_image_pin,
            Some(andler_core::BaseImagePin {
                id: "android13-vanilla-2099-01-01T00:00:00Z".to_string(),
                sha256: "ab".repeat(32),
            })
        );

        let crate::create::ProtoRequest::Android(req) = creation.into_request() else {
            panic!("expected an Android request");
        };
        let pin = req.profile.expect("profile").base_image_pin.expect("pin");
        assert_eq!(pin.id, "android13-vanilla-2099-01-01T00:00:00Z");
        assert_eq!(pin.sha256, "ab".repeat(32));
    }

    #[test]
    fn android_arm_translator_field_selects_libhoudini() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\narm_translator = \"libhoudini\"\n");
        let cfg = resolve(&toml);
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.arm_translator, ArmTranslator::Libhoudini);
    }

    #[test]
    fn android_arm_translator_field_wins_over_legacy_libndk() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\nlibndk = true\narm_translator = \"none\"\n");
        let cfg = resolve(&toml);
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.arm_translator, ArmTranslator::None);
    }

    #[test]
    fn android_unknown_arm_translator_is_rejected_not_silently_none() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\narm_translator = \"hibridge\"\n");
        let err = draft_result(&toml).expect_err("an unknown translator must be rejected");
        assert!(
            matches!(
                err,
                InstanceFileError::UnsupportedValue {
                    field: "arm_translator",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn android_linux_only_sections_are_rejected_not_silently_dropped() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\n[cpu]\ncores = 16\nsockets = 1\nthreads = 1\npriority = \"normal\"\n");
        let err = resolve_err(&toml);
        assert!(err.contains("`cpu` section"), "{err}");
    }

    #[test]
    fn android_autostart_is_rejected_not_silently_dropped() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\nautostart = true\n");
        let err = resolve_err(&toml);
        assert!(err.contains("autostart"), "{err}");
    }

    #[test]
    fn android_no_translator_fields_defaults_to_none() {
        let cfg = resolve(MINIMAL_ANDROID_TOML);
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.arm_translator, ArmTranslator::None);
    }

    #[test]
    fn android_version_11_parses() {
        let toml = MINIMAL_ANDROID_TOML.replace("android_version = 13", "android_version = 11");
        let cfg = resolve(&toml);
        let InstanceKind::AndroidVm { android_profile } = cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.android_version, AndroidVersion::Android11);
    }

    #[test]
    fn android_version_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            android_version = 13
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let cfg = resolve(toml);
        assert!(matches!(cfg.kind, InstanceKind::AndroidVm { .. }));
    }

    #[test]
    fn base_image_path_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let cfg = resolve(toml);
        assert!(matches!(cfg.kind, InstanceKind::AndroidVm { .. }));
    }

    #[test]
    fn no_android_fields_triggers_linux_mode() {
        let cfg = resolve(MINIMAL_LINUX_TOML);
        assert!(matches!(cfg.kind, InstanceKind::LinuxVm { .. }));
    }

    #[test]
    fn a_missing_linux_disk_path_is_reported_by_the_draft() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let err = draft_result(toml).expect_err("missing disk_path must be reported");
        assert!(
            matches!(err, InstanceFileError::MissingField { field: "disk_path" }),
            "{err}"
        );
    }

    #[test]
    fn into_draft_reports_missing_iso_path() {
        let toml = r#"
            name = "test-vm"
            disk_path = "/tmp/disk.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let err = draft_result(toml).expect_err("missing iso_path must be reported");
        assert!(
            matches!(err, InstanceFileError::MissingField { field: "iso_path" }),
            "{err}"
        );
    }

    #[test]
    fn into_draft_rejects_unsupported_android_version() {
        let toml = r#"
            name = "test-vm"
            android_version = 12
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let err = draft_result(toml).expect_err("unsupported android_version must be rejected");
        assert!(
            matches!(
                err,
                InstanceFileError::UnsupportedValue {
                    field: "android_version",
                    ..
                }
            ),
            "{err}"
        );
    }

    fn draft_result(toml_text: &str) -> Result<InstanceFileDraft, InstanceFileError> {
        let file: InstanceFile = toml::from_str(toml_text).expect("TOML must parse");
        file.into_draft()
    }

    #[test]
    fn load_reports_read_error_for_missing_file() {
        let err = InstanceFile::load(Path::new("/nonexistent/path/instance.toml"))
            .expect_err("loading a missing file must fail");
        assert!(matches!(err, InstanceFileError::Read { .. }));
    }

    #[test]
    fn load_reports_parse_error_for_invalid_toml() {
        let dir = std::env::temp_dir().join(format!("andler-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("invalid.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let err = InstanceFile::load(&path).expect_err("invalid TOML must fail to parse");
        assert!(matches!(err, InstanceFileError::Parse { .. }));

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn load_rejects_nonexistent_iso_path() {
        let dir =
            std::env::temp_dir().join(format!("andler-cli-test-badpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("instance.toml");
        std::fs::write(
            &path,
            r#"
                name = "test-vm"
                iso_path = "/nonexistent/andler-test/does-not-exist.iso"
                disk_path = "disk.qcow2"
                ovmf_vars_path = "/tmp/test_VARS.fd"
            "#,
        )
        .unwrap();

        let err = InstanceFile::load(&path).expect_err("nonexistent iso_path must be rejected");
        assert!(matches!(
            err,
            InstanceFileError::InvalidPath {
                field: "iso_path",
                ..
            }
        ));

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn load_canonicalizes_iso_path_and_disk_parent() {
        let dir =
            std::env::temp_dir().join(format!("andler-cli-test-canon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let iso_path = dir.join("test.iso");
        std::fs::write(&iso_path, b"fake iso contents").unwrap();
        let vars_path = dir.join("VARS.fd");
        std::fs::write(&vars_path, b"fake ovmf vars").unwrap();
        let toml_path = dir.join("instance.toml");
        let messy_iso = dir
            .join("..")
            .join(dir.file_name().unwrap())
            .join("test.iso");
        std::fs::write(
            &toml_path,
            format!(
                r#"
                    name = "test-vm"
                    iso_path = {:?}
                    disk_path = "new-disk.qcow2"
                    ovmf_vars_path = {:?}
                "#,
                messy_iso.to_string_lossy(),
                vars_path.to_string_lossy()
            ),
        )
        .unwrap();

        let file = InstanceFile::load(&toml_path).expect("load must succeed");
        let resolved_iso = file.iso_path.expect("iso_path must be set");
        assert!(!resolved_iso.to_string_lossy().contains(".."));
        assert_eq!(resolved_iso, std::fs::canonicalize(&iso_path).unwrap());

        let resolved_disk = file.disk_path.expect("disk_path must be set");
        assert_eq!(resolved_disk.file_name().unwrap(), "new-disk.qcow2");
        assert!(!resolved_disk.to_string_lossy().contains(".."));

        std::fs::remove_file(&iso_path).ok();
        std::fs::remove_file(&vars_path).ok();
        std::fs::remove_file(&toml_path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn load_rejects_nonexistent_ovmf_vars_path() {
        let dir =
            std::env::temp_dir().join(format!("andler-cli-test-badvars-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let iso_path = dir.join("test.iso");
        std::fs::write(&iso_path, b"fake iso contents").unwrap();
        let toml_path = dir.join("instance.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
                    name = "test-vm"
                    iso_path = {:?}
                    disk_path = "disk.qcow2"
                    ovmf_vars_path = "/nonexistent/andler-test/does-not-exist_VARS.fd"
                "#,
                iso_path.to_string_lossy()
            ),
        )
        .unwrap();

        let err = InstanceFile::load(&toml_path)
            .expect_err("nonexistent ovmf_vars_path must be rejected");
        assert!(matches!(
            err,
            InstanceFileError::InvalidPath {
                field: "ovmf_vars_path",
                ..
            }
        ));

        std::fs::remove_file(&iso_path).ok();
        std::fs::remove_file(&toml_path).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
