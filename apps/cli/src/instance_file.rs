use std::path::{Path, PathBuf};

use andler_core::{
    AudioConfig, CdromBus, CpuConfig, DiskConfig, DisplayConfig, GpuConfig, InputConfig,
    MemoryConfig, NetworkConfig,
};
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
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

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // carries full proto requests; boxing would complicate callers
pub enum InstanceFileResult {
    Linux(CreateInstanceRequest),

    Android(CreateAndroidInstanceRequest),
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

    pub ovmf_vars_path: PathBuf,

    #[serde(default = "default_true")]
    pub enable_uefi: bool,

    #[serde(default)]
    pub android_version: Option<u32>,

    #[serde(default)]
    pub base_image_path: Option<String>,

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

        self.ovmf_vars_path = std::fs::canonicalize(&self.ovmf_vars_path).map_err(|source| {
            InstanceFileError::InvalidPath {
                field: "ovmf_vars_path",
                path: self.ovmf_vars_path.clone(),
                source,
            }
        })?;

        Ok(())
    }

    pub fn into_result(self) -> Result<InstanceFileResult, InstanceFileError> {
        if self.android_version.is_some() || self.base_image_path.is_some() {
            Ok(InstanceFileResult::Android(self.into_android_request()?))
        } else {
            Ok(InstanceFileResult::Linux(self.into_request()?))
        }
    }

    fn into_request(self) -> Result<CreateInstanceRequest, InstanceFileError> {
        let disk_path = self
            .disk_path
            .clone()
            .ok_or(InstanceFileError::MissingField { field: "disk_path" })?;
        let mut disk = DiskConfig::reference_default(disk_path);
        if let Some(gib) = self.disk_size_gib {
            disk.size_bytes =
                gib.checked_mul(DiskConfig::GIB)
                    .ok_or(InstanceFileError::UnsupportedValue {
                        field: "disk_size_gib",
                        value: gib.to_string(),
                    })?;
        }
        disk.snapshot_timeout_secs = self.snapshot_timeout_secs;
        disk.compact_on_shutdown = self.compact_on_shutdown;

        let iso_path = self
            .iso_path
            .clone()
            .ok_or(InstanceFileError::MissingField { field: "iso_path" })?;
        let resolved_cdrom_bus = match self.cdrom_bus {
            InstanceFileCdromBus::Auto => CdromBus::recommended_for_iso_filename(&iso_path),
            InstanceFileCdromBus::Virtio => CdromBus::VirtioScsi,
            InstanceFileCdromBus::Ide => CdromBus::Ide,
        };

        let mut req = CreateInstanceRequest {
            name: self.name,
            iso_path: path_to_string(&iso_path),
            cpu: Some(self.cpu.unwrap_or_else(CpuConfig::reference_default).into()),
            memory: Some(
                self.memory
                    .unwrap_or_else(MemoryConfig::reference_default)
                    .into(),
            ),
            disk: Some(disk.into()),
            display: Some(
                self.display
                    .unwrap_or_else(DisplayConfig::reference_default)
                    .into(),
            ),
            gpu: Some(self.gpu.unwrap_or_else(GpuConfig::reference_default).into()),
            network: Some(
                self.network
                    .unwrap_or_else(NetworkConfig::reference_default)
                    .into(),
            ),
            firmware: Some(
                andler_core::FirmwareConfig {
                    enable_uefi: self.enable_uefi,
                    ovmf_code_path: std::path::PathBuf::new(),
                    ovmf_vars_path: self.ovmf_vars_path.clone(),
                }
                .into(),
            ),
            audio: Some(
                self.audio
                    .unwrap_or_else(AudioConfig::reference_default)
                    .into(),
            ),
            input: Some(
                self.input
                    .unwrap_or_else(InputConfig::reference_default)
                    .into(),
            ),
            ..Default::default()
        };
        req.set_cdrom_bus(resolved_cdrom_bus.into());
        Ok(req)
    }

    fn into_android_request(self) -> Result<CreateAndroidInstanceRequest, InstanceFileError> {
        let base_image_path = self.base_image_path.unwrap_or_default();

        let overlay_size_bytes = self
            .overlay_size_gib
            .unwrap_or(128)
            .checked_mul(1024 * 1024 * 1024)
            .ok_or(InstanceFileError::UnsupportedValue {
                field: "overlay_size_gib",
                value: self.overlay_size_gib.unwrap_or(128).to_string(),
            })?;

        let instances_root = self.instances_root.unwrap_or_else(default_instances_root);

        let android_version = self.android_version.unwrap_or(13);
        let mut profile = ProtoAndroidProfile {
            gapps: self.gapps,
            microg: self.microg,
            ..Default::default()
        };
        let android_version = match android_version {
            11 => andler_rpc::proto::AndroidVersion::Android11,
            13 => andler_rpc::proto::AndroidVersion::Android13,
            other => {
                return Err(InstanceFileError::UnsupportedValue {
                    field: "android_version",
                    value: other.to_string(),
                })
            }
        };
        profile.set_android_version(android_version);

        let arm_translator = match self.arm_translator.as_deref() {
            Some("libndk") => andler_rpc::proto::ArmTranslator::Libndk,
            Some("libhoudini") => andler_rpc::proto::ArmTranslator::Libhoudini,
            Some(_) => andler_rpc::proto::ArmTranslator::None,
            None if self.libndk => andler_rpc::proto::ArmTranslator::Libndk,
            None => andler_rpc::proto::ArmTranslator::None,
        };
        profile.set_arm_translator(arm_translator);

        Ok(CreateAndroidInstanceRequest {
            name: self.name,
            profile: Some(profile),
            base_image_path,
            instances_root,
            overlay_size_bytes,
            ovmf_vars_template: path_to_string(&self.ovmf_vars_path),
            linked_overlay: self.linked_overlay,
        })
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
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
    use super::*;

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
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("minimal Linux TOML must parse");
        let result = file.into_result().unwrap();
        match result {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.name, "test-vm");
                assert_eq!(req.iso_path, "/tmp/test.iso");
                assert!(req.cpu.is_some());
                assert!(req.memory.is_some());
                assert!(req.disk.is_some());
                assert!(req.display.is_some());
                assert!(req.gpu.is_some());
                assert!(req.network.is_some());
                assert!(req.firmware.is_some());
                assert!(req.audio.is_some());
                assert!(req.input.is_some());
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_disk_size_gib_overrides_default() {
        let toml = format!("{MINIMAL_LINUX_TOML}\ndisk_size_gib = 100\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                let disk = req.disk.expect("disk must be Some");
                assert_eq!(disk.size_bytes, 100 * 1024 * 1024 * 1024);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_compact_on_shutdown_defaults_to_false() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("minimal Linux TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                let disk = req.disk.expect("disk must be Some");
                assert!(
                    !disk.compact_on_shutdown,
                    "compact_on_shutdown must default to false when absent from TOML"
                );
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_compact_on_shutdown_can_be_enabled() {
        let toml = format!("{MINIMAL_LINUX_TOML}\ncompact_on_shutdown = true\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                let disk = req.disk.expect("disk must be Some");
                assert!(disk.compact_on_shutdown);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_cdrom_bus_defaults_to_auto_detect_by_iso_filename() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("minimal Linux TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.cdrom_bus(), andler_rpc::proto::CdromBus::Ide);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_cdrom_bus_auto_detects_virtio_for_known_distro_filename() {
        let toml = MINIMAL_LINUX_TOML.replace("/tmp/test.iso", "/tmp/ubuntu-24.04.iso");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.cdrom_bus(), andler_rpc::proto::CdromBus::VirtioScsi);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_cdrom_bus_explicit_choice_overrides_auto_detect() {
        let toml = MINIMAL_LINUX_TOML.replace("/tmp/test.iso", "/tmp/ubuntu-24.04.iso")
            + "\ncdrom_bus = \"ide\"\n";
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.cdrom_bus(), andler_rpc::proto::CdromBus::Ide);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn minimal_android_file_parses() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_ANDROID_TOML).expect("minimal Android TOML must parse");
        let result = file.into_result().unwrap();
        match result {
            InstanceFileResult::Android(req) => {
                assert_eq!(req.name, "test-android");
                assert_eq!(req.base_image_path, "/tmp/base.qcow2");
                assert_eq!(
                    req.overlay_size_bytes,
                    128 * 1024 * 1024 * 1024,
                    "overlay default must be 128 GiB when overlay_size_gib is omitted"
                );
                assert!(req.profile.is_some());
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.android_version(),
                    andler_rpc::proto::AndroidVersion::Android13
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
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
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Android(req) => {
                assert_eq!(req.overlay_size_bytes, 30 * 1024 * 1024 * 1024);
                let profile = req.profile.unwrap();
                assert!(profile.gapps);
                assert!(profile.microg);
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::Libndk
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_arm_translator_field_selects_libhoudini() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\narm_translator = \"libhoudini\"\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::Libhoudini
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_arm_translator_field_wins_over_legacy_libndk() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\nlibndk = true\narm_translator = \"none\"\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::None
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_no_translator_fields_defaults_to_none() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_ANDROID_TOML).expect("minimal Android TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::None
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_version_11_parses() {
        let toml = MINIMAL_ANDROID_TOML.replace("android_version = 13", "android_version = 11");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result().unwrap() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.android_version(),
                    andler_rpc::proto::AndroidVersion::Android11
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_version_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            android_version = 13
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        assert!(matches!(
            file.into_result().unwrap(),
            InstanceFileResult::Android(_)
        ));
    }

    #[test]
    fn base_image_path_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        assert!(matches!(
            file.into_result().unwrap(),
            InstanceFileResult::Android(_)
        ));
    }

    #[test]
    fn no_android_fields_triggers_linux_mode() {
        let file: InstanceFile = toml::from_str(MINIMAL_LINUX_TOML).expect("TOML must parse");
        assert!(matches!(
            file.into_result().unwrap(),
            InstanceFileResult::Linux(_)
        ));
    }

    #[test]
    fn missing_required_linux_field_fails() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
        "#;
        let result: Result<InstanceFile, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn into_result_reports_missing_disk_path() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        let err = file
            .into_result()
            .expect_err("missing disk_path must be reported, not panic");
        assert!(matches!(
            err,
            InstanceFileError::MissingField { field: "disk_path" }
        ));
    }

    #[test]
    fn into_result_reports_missing_iso_path() {
        let toml = r#"
            name = "test-vm"
            disk_path = "/tmp/disk.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        let err = file
            .into_result()
            .expect_err("missing iso_path must be reported, not panic");
        assert!(matches!(
            err,
            InstanceFileError::MissingField { field: "iso_path" }
        ));
    }

    #[test]
    fn into_result_rejects_unsupported_android_version() {
        let toml = r#"
            name = "test-vm"
            android_version = 12
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        let err = file
            .into_result()
            .expect_err("unsupported android_version must be rejected");
        assert!(matches!(
            err,
            InstanceFileError::UnsupportedValue {
                field: "android_version",
                ..
            }
        ));
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
