//! Configuration file for `andler create` (TOML).
//!
//! Supports both LinuxVm and AndroidVm creation from a single TOML format.
//! The type is auto-detected: if `android_version` (or `base_image_path`)
//! is present, it's an AndroidVm; otherwise LinuxVm.
//!
//! LinuxVm required fields: `name`, `iso_path`, `disk_path`, `ovmf_vars_path`.
//! AndroidVm required fields: `name`, `android_version`, `base_image_path`, `ovmf_vars_path`.
//!
//! Each section (`cpu`/`memory`/`display`/`gpu`/`network`/`audio`/`input`)
//! is optional — absence means `reference_default()`.

use std::path::{Path, PathBuf};

use andler_core::{
    AudioConfig, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
    InputConfig, MemoryConfig, NetworkConfig,
};
use andler_rpc::proto::{
    CreateAndroidInstanceRequest, CreateInstanceRequest, AndroidProfile as ProtoAndroidProfile,
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
}

/// Result of parsing an instance TOML file — either a LinuxVm or AndroidVm request.
#[derive(Debug)]
pub enum InstanceFileResult {
    /// LinuxVm — calls `CreateInstance` RPC.
    Linux(CreateInstanceRequest),
    /// AndroidVm — calls `CreateAndroidInstance` RPC.
    Android(CreateAndroidInstanceRequest),
}

/// TOML-представление выбора `cdrom_bus` — отдельный тип от
/// `andler_core::CdromBus`, потому что у него есть третье состояние
/// (`Auto`), которого нет (и не должно быть) в домене: домен всегда несёт
/// уже принятое решение, а "auto" — это просьба к этому модулю принять
/// решение за пользователя на основе `recommended_for_iso_filename`, не
/// валидный домена сам по себе.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceFileCdromBus {
    #[default]
    Auto,
    Virtio,
    Ide,
}

/// TOML instance file. Shared fields for both LinuxVm and AndroidVm.
/// Android-specific fields are `Option` — if present, the file is treated
/// as an AndroidVm config.
#[derive(Debug, Deserialize)]
pub struct InstanceFile {
    pub name: String,

    // --- Linux-specific (required for LinuxVm) ---
    /// Path to installer ISO. Required for LinuxVm, ignored for AndroidVm.
    #[serde(default)]
    pub iso_path: Option<PathBuf>,
    /// Path to disk file. Required for LinuxVm, ignored for AndroidVm.
    #[serde(default)]
    pub disk_path: Option<PathBuf>,
    /// Disk size in GiB. Optional (default: 256 GiB — see
    /// `DiskConfig::reference_default`; thin-provisioned qcow2, so this
    /// is a nominal upper bound, not space used immediately on the host).
    #[serde(default)]
    pub disk_size_gib: Option<u64>,
    /// Automatically compact the disk after every graceful shutdown.
    /// Optional (default: `false` — see
    /// `DiskConfig::compact_on_shutdown`; off unless explicitly enabled,
    /// has no effect on non-qcow2 disks).
    #[serde(default)]
    pub compact_on_shutdown: bool,
    /// Bus for the ISO/CD-ROM drive. Optional — absent or `"auto"` means
    /// decide based on the ISO filename (see
    /// `CdromBus::recommended_for_iso_filename`); `"virtio"` or `"ide"`
    /// force an explicit choice. See PLAN.md, "Монтирование ISO /
    /// CD-ROM".
    #[serde(default)]
    pub cdrom_bus: InstanceFileCdromBus,

    // --- Common ---
    /// Path to OVMF_VARS template. Required for both types.
    pub ovmf_vars_path: PathBuf,

    // --- Android-specific (presence = AndroidVm) ---
    /// Android version. If present, this is an AndroidVm config.
    #[serde(default)]
    pub android_version: Option<u32>,
    /// Path to Android base image. Required for AndroidVm.
    #[serde(default)]
    pub base_image_path: Option<String>,
    /// Overlay disk size in GiB (default: 20).
    #[serde(default)]
    pub overlay_size_gib: Option<u64>,
    /// Root mode: "none" (default), "magisk".
    #[serde(default)]
    pub root: Option<String>,
    /// Path to Magisk binaries directory. Required when root = "magisk".
    #[serde(default)]
    pub magisk_dir: Option<PathBuf>,
    /// Include Google Apps.
    #[serde(default)]
    pub gapps: bool,
    /// Include microG.
    #[serde(default)]
    pub microg: bool,
    /// Include ARM→x86 translation (libhoudini/libndk). **Deprecated** —
    /// use `arm_translator = "libndk"` instead. Kept only for backward
    /// compatibility with TOML files written before `arm_translator`
    /// existed: `libndk = true` -> `ArmTranslator::Libndk` when
    /// `arm_translator` is absent. If both are present, `arm_translator`
    /// wins.
    #[serde(default)]
    pub libndk: bool,
    /// ARM→x86 translator: `"none"` (default), `"libndk"`, `"libhoudini"`.
    #[serde(default)]
    pub arm_translator: Option<String>,
    /// Instance directory root for Android instances.
    #[serde(default)]
    pub instances_root: Option<String>,

    // --- Optional config sections ---
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
    /// Snapshot timeout in seconds (default: 30).
    #[serde(default)]
    pub snapshot_timeout_secs: Option<u64>,
}

impl InstanceFile {
    /// Reads and parses a TOML file.
    pub fn load(path: &Path) -> Result<Self, InstanceFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| InstanceFileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| InstanceFileError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Determines the instance type and returns the appropriate request.
    pub fn into_result(self) -> InstanceFileResult {
        if self.android_version.is_some() || self.base_image_path.is_some() {
            InstanceFileResult::Android(self.into_android_request())
        } else {
            InstanceFileResult::Linux(self.into_request())
        }
    }

    /// Builds a `CreateInstanceRequest` for LinuxVm.
    fn into_request(self) -> CreateInstanceRequest {
        let disk_path = self.disk_path.expect("disk_path required for LinuxVm");
        let mut disk = DiskConfig::reference_default(disk_path);
        if let Some(gib) = self.disk_size_gib {
            disk.size_bytes = gib.checked_mul(DiskConfig::GIB)
                .expect("disk size overflow");
        }
        disk.snapshot_timeout_secs = self.snapshot_timeout_secs;
        disk.compact_on_shutdown = self.compact_on_shutdown;

        let iso_path = self.iso_path.expect("iso_path required for LinuxVm");
        let resolved_cdrom_bus = match self.cdrom_bus {
            InstanceFileCdromBus::Auto => CdromBus::recommended_for_iso_filename(&iso_path),
            InstanceFileCdromBus::Virtio => CdromBus::VirtioScsi,
            InstanceFileCdromBus::Ide => CdromBus::Ide,
        };

        let mut req = CreateInstanceRequest {
            name: self.name,
            iso_path: path_to_string(&iso_path),
            cpu: Some(
                self.cpu
                    .unwrap_or_else(CpuConfig::reference_default)
                    .into(),
            ),
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
                // `ovmf_code_path` намеренно пустой — daemon (service.rs)
                // подставит авто-определённый или явный путь из своей
                // конфигурации. CLI знает только путь к VARS (персональный
                // per-instance файл), но не путь к CODE (системный,
                // зависящий от дистрибутива). Пустая строка = "используй
                // авто-детект daemon'а" — это соглашение между CLI и
                // service.rs, задокументированное в обоих местах.
                andler_core::FirmwareConfig {
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
        req
    }

    /// Builds a `CreateAndroidInstanceRequest` for AndroidVm.
    fn into_android_request(self) -> CreateAndroidInstanceRequest {
        let base_image_path = self
            .base_image_path
            .expect("base_image_path required for AndroidVm");

        let overlay_size_bytes = self
            .overlay_size_gib
            .unwrap_or(20)
            .checked_mul(1024 * 1024 * 1024)
            .expect("overlay size overflow");

        let instances_root = self
            .instances_root
            .unwrap_or_else(default_instances_root);

        let magisk_dir = self
            .magisk_dir
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Build AndroidProfile
        let android_version = self.android_version.unwrap_or(13);
        let mut profile = ProtoAndroidProfile {
            gapps: self.gapps,
            microg: self.microg,
            ..Default::default()
        };
        profile.set_android_version(match android_version {
            11 => andler_rpc::proto::AndroidVersion::Android11,
            _ => andler_rpc::proto::AndroidVersion::Android13,
        });

        // `arm_translator` побеждает, если задан явно; иначе — обратная
        // совместимость со старым `libndk = true/false` (см. doc-
        // комментарий на поле `libndk`).
        let arm_translator = match self.arm_translator.as_deref() {
            Some("libndk") => andler_rpc::proto::ArmTranslator::Libndk,
            Some("libhoudini") => andler_rpc::proto::ArmTranslator::Libhoudini,
            Some(_) => andler_rpc::proto::ArmTranslator::ArmTranslatorNone,
            None if self.libndk => andler_rpc::proto::ArmTranslator::Libndk,
            None => andler_rpc::proto::ArmTranslator::ArmTranslatorNone,
        };
        profile.set_arm_translator(arm_translator);

        let root_mode = match self.root.as_deref() {
            Some("magisk") => andler_rpc::proto::RootMode::Magisk,
            _ => andler_rpc::proto::RootMode::None,
        };
        profile.set_root(root_mode);

        CreateAndroidInstanceRequest {
            name: self.name,
            profile: Some(profile),
            base_image_path,
            instances_root,
            overlay_size_bytes,
            ovmf_vars_template: path_to_string(&self.ovmf_vars_path),
            magisk_dir,
        }
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

#[cfg(test)]
mod tests {
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

    // --- LinuxVm tests ---

    #[test]
    fn minimal_linux_file_parses_and_fills_every_section() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("minimal Linux TOML must parse");
        let result = file.into_result();
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
        match file.into_result() {
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
        match file.into_result() {
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
        match file.into_result() {
            InstanceFileResult::Linux(req) => {
                let disk = req.disk.expect("disk must be Some");
                assert!(disk.compact_on_shutdown);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_cdrom_bus_defaults_to_auto_detect_by_iso_filename() {
        // MINIMAL_LINUX_TOML's iso_path is "/tmp/test.iso" — not a known
        // distro name, so auto-detect should fall back to Ide.
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("minimal Linux TOML must parse");
        match file.into_result() {
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
        match file.into_result() {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.cdrom_bus(), andler_rpc::proto::CdromBus::VirtioScsi);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    #[test]
    fn linux_cdrom_bus_explicit_choice_overrides_auto_detect() {
        // Explicit "ide" must win even though the filename would
        // auto-detect to virtio.
        let toml = MINIMAL_LINUX_TOML
            .replace("/tmp/test.iso", "/tmp/ubuntu-24.04.iso")
            + "\ncdrom_bus = \"ide\"\n";
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result() {
            InstanceFileResult::Linux(req) => {
                assert_eq!(req.cdrom_bus(), andler_rpc::proto::CdromBus::Ide);
            }
            other => panic!("expected Linux, got {other:?}"),
        }
    }

    // --- AndroidVm tests ---

    #[test]
    fn minimal_android_file_parses() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_ANDROID_TOML).expect("minimal Android TOML must parse");
        let result = file.into_result();
        match result {
            InstanceFileResult::Android(req) => {
                assert_eq!(req.name, "test-android");
                assert_eq!(req.base_image_path, "/tmp/base.qcow2");
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
             root = \"magisk\"\n\
             magisk_dir = \"/tmp/magisk\"\n\
             gapps = true\n\
             microg = true\n\
             libndk = true\n"
        );
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result() {
            InstanceFileResult::Android(req) => {
                assert_eq!(req.overlay_size_bytes, 30 * 1024 * 1024 * 1024);
                assert_eq!(req.magisk_dir, "/tmp/magisk");
                let profile = req.profile.unwrap();
                assert!(profile.gapps);
                assert!(profile.microg);
                // Backward compat: старый `libndk = true` (без
                // `arm_translator`) резолвится в Libndk.
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::Libndk
                );
                assert_eq!(
                    profile.root(),
                    andler_rpc::proto::RootMode::Magisk
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_arm_translator_field_selects_libhoudini() {
        let toml = format!("{MINIMAL_ANDROID_TOML}\narm_translator = \"libhoudini\"\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result() {
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
        // Явный arm_translator побеждает над устаревшим libndk, даже
        // если они противоречат друг другу (рассинхронизированный файл).
        let toml = format!(
            "{MINIMAL_ANDROID_TOML}\nlibndk = true\narm_translator = \"none\"\n"
        );
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::ArmTranslatorNone
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_no_translator_fields_defaults_to_none() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_ANDROID_TOML).expect("minimal Android TOML must parse");
        match file.into_result() {
            InstanceFileResult::Android(req) => {
                let profile = req.profile.unwrap();
                assert_eq!(
                    profile.arm_translator(),
                    andler_rpc::proto::ArmTranslator::ArmTranslatorNone
                );
            }
            other => panic!("expected Android, got {other:?}"),
        }
    }

    #[test]
    fn android_version_11_parses() {
        let toml = MINIMAL_ANDROID_TOML.replace("android_version = 13", "android_version = 11");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        match file.into_result() {
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

    // --- Auto-detect tests ---

    #[test]
    fn android_version_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            android_version = 13
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        assert!(matches!(file.into_result(), InstanceFileResult::Android(_)));
    }

    #[test]
    fn base_image_path_field_triggers_android_mode() {
        let toml = r#"
            name = "test"
            base_image_path = "/tmp/base.qcow2"
            ovmf_vars_path = "/tmp/VARS.fd"
        "#;
        let file: InstanceFile = toml::from_str(toml).expect("TOML must parse");
        assert!(matches!(file.into_result(), InstanceFileResult::Android(_)));
    }

    #[test]
    fn no_android_fields_triggers_linux_mode() {
        let file: InstanceFile =
            toml::from_str(MINIMAL_LINUX_TOML).expect("TOML must parse");
        assert!(matches!(file.into_result(), InstanceFileResult::Linux(_)));
    }

    // --- Error tests ---

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
    fn load_reports_read_error_for_missing_file() {
        let err = InstanceFile::load(Path::new("/nonexistent/path/instance.toml"))
            .expect_err("loading a missing file must fail");
        assert!(matches!(err, InstanceFileError::Read { .. }));
    }

    #[test]
    fn load_reports_parse_error_for_invalid_toml() {
        let dir = std::env::temp_dir().join(format!(
            "andler-cli-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("invalid.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let err = InstanceFile::load(&path).expect_err("invalid TOML must fail to parse");
        assert!(matches!(err, InstanceFileError::Parse { .. }));

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
