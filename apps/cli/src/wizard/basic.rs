use std::path::Path;

use inquire::{CustomType, Select, Text};

use crate::CliAndroidVersion;

use super::{advanced::ask_gapps, map_inquire_err, WizardError, WizardKind};

const DEFAULT_DISK_GIB: u64 = 256;
const MAX_DISK_GIB: u64 = 65536;

#[derive(Debug, Clone)]
pub struct LinuxBasicResult {
    pub name: String,
    pub iso_path: String,
    pub disk_size_gib: u64,
    pub instances_root: String,
    pub enable_uefi: bool,
}

#[derive(Debug, Clone)]
pub struct AndroidBasicResult {
    pub name: String,
    pub base_image: String,
    pub base_image_auto_resolved: bool,
    pub android_version: CliAndroidVersion,
    pub gapps: bool,
    pub disk_size_gib: u64,
    pub instances_root: String,
    pub linked: bool,
}

#[derive(Debug, Clone)]
pub enum BasicResult {
    Linux(LinuxBasicResult),
    Android(AndroidBasicResult),
}

#[allow(dead_code)] // kind(), disk_size_gib(), instances_root() only used in tests; name() used in summary
impl BasicResult {
    pub fn name(&self) -> &str {
        match self {
            Self::Linux(r) => &r.name,
            Self::Android(r) => &r.name,
        }
    }

    pub fn kind(&self) -> WizardKind {
        match self {
            Self::Linux(_) => WizardKind::Linux,
            Self::Android(_) => WizardKind::Android,
        }
    }

    pub fn disk_size_gib(&self) -> u64 {
        match self {
            Self::Linux(r) => r.disk_size_gib,
            Self::Android(r) => r.disk_size_gib,
        }
    }

    pub fn instances_root(&self) -> &str {
        match self {
            Self::Linux(r) => &r.instances_root,
            Self::Android(r) => &r.instances_root,
        }
    }
}

pub fn run_linux(
    name: String,
    iso_path: Option<String>,
    instances_root: Option<String>,
) -> Result<LinuxBasicResult, WizardError> {
    let iso = ask_iso_path(iso_path)?;
    let disk_size_gib = ask_disk_size(DEFAULT_DISK_GIB)?;
    let instances_root = instances_root.unwrap_or_else(default_instances_root);
    let enable_uefi = ask_enable_uefi()?;
    Ok(LinuxBasicResult {
        name,
        iso_path: iso,
        disk_size_gib,
        instances_root,
        enable_uefi,
    })
}

pub fn run_android(
    name: String,
    base_image_path: Option<String>,
    instances_root: Option<String>,
) -> Result<AndroidBasicResult, WizardError> {
    let android_version = ask_android_version()?;
    let gapps = ask_gapps(None)?;
    let (base_image, base_image_auto_resolved) =
        ask_base_image(base_image_path, android_version, gapps)?;
    let disk_size_gib = ask_disk_size(DEFAULT_DISK_GIB)?;
    let instances_root = instances_root.unwrap_or_else(default_instances_root);
    Ok(AndroidBasicResult {
        name,
        base_image,
        base_image_auto_resolved,
        android_version,
        gapps,
        disk_size_gib,
        instances_root,
        linked: false,
    })
}

pub fn ask_kind(prefilled: Option<WizardKind>) -> Result<WizardKind, WizardError> {
    if let Some(k) = prefilled {
        return Ok(k);
    }
    let choice = Select::new("VM type:", vec!["Linux", "Android"])
        .with_help_message(
            "Linux — any distro with ISO; Android — Android with VirtIO-GPU and Waydroid",
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(if choice == "Linux" {
        WizardKind::Linux
    } else {
        WizardKind::Android
    })
}

pub fn ask_name(prefilled: Option<String>, kind: WizardKind) -> Result<String, WizardError> {
    if let Some(n) = prefilled {
        return Ok(n);
    }
    let placeholder = match kind {
        WizardKind::Linux => "my-linux-vm",
        WizardKind::Android => "my-android-vm",
    };
    Text::new("VM name:")
        .with_placeholder(placeholder)
        .with_validator(|s: &str| validate_name(s))
        .prompt()
        .map_err(map_inquire_err)
}

pub fn ask_iso_path(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        validate_iso_path(&p)?;
        return Ok(p);
    }
    let path = Text::new("Path to ISO image (Enter — skip, boot from disk):")
        .with_placeholder("/home/user/isos/cachyos.iso")
        .with_help_message(
            "Enter path to .iso file, or press Enter to skip (boot from existing disk)",
        )
        .with_default("")
        .with_validator(|s: &str| match validate_iso_path(s) {
            Ok(()) => Ok(inquire::validator::Validation::Valid),
            Err(e) => Ok(inquire::validator::Validation::Invalid(e.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;
    validate_iso_path(&path)?;
    Ok(path)
}

pub fn ask_base_image(
    prefilled: Option<String>,
    android_version: CliAndroidVersion,
    gapps: bool,
) -> Result<(String, bool), WizardError> {
    if let Some(p) = prefilled {
        validate_base_image_path(&p)?;
        return Ok((p, false));
    }

    let quick_profile = andler_core::AndroidProfile {
        android_version: android_version.into(),
        gapps,
        microg: false,
        arm_translator: andler_core::ArmTranslator::None,
        boot_mode: andler_core::AndroidBootMode::Android,
        base_image_pin: None,
    };
    let suggested = andler_core::base_image::resolve(&quick_profile)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    let no_match_help = format!(
        "No matching {} image in ~/.andler/cache/base-images/ — build one with \
         docker/images/build.sh, or point to one manually",
        if gapps { "GApps" } else { "VANILLA" }
    );

    let mut prompt = Text::new("Path to Android base image:")
        .with_placeholder("/path/to/android-base.qcow2")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "Base image path is required".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        });
    prompt = match &suggested {
        Some(found) => prompt.with_default(found).with_help_message(
            "Found in ~/.andler/cache/base-images/ — press Enter to use it, \
             or type a different path",
        ),
        None => prompt.with_help_message(&no_match_help),
    };

    let path = prompt.prompt().map_err(map_inquire_err)?;
    validate_base_image_path(&path)?;
    let auto_resolved = suggested.as_deref() == Some(path.as_str());
    Ok((path, auto_resolved))
}

pub fn ask_android_version() -> Result<CliAndroidVersion, WizardError> {
    let choice = Select::new(
        "Android version:",
        vec!["Android 13 (recommended)", "Android 11"],
    )
    .with_help_message("Android 13 has better VirtIO-GPU/Venus support")
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("Android 13") {
        CliAndroidVersion::Android13
    } else {
        CliAndroidVersion::Android11
    })
}

pub fn ask_disk_size(default_gib: u64) -> Result<u64, WizardError> {
    CustomType::<u64>::new("Disk size (GiB):")
        .with_default(default_gib)
        .with_help_message("Thin-provisioned qcow2 — nominal limit, not actual host usage")
        .with_formatter(&|v: u64| format!("{v} GiB"))
        .with_error_message("Enter an integer between 1 and 65536, e.g. 256")
        .with_validator(|v: &u64| {
            if (1..=MAX_DISK_GIB).contains(v) {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    format!("Enter an integer between 1 and {MAX_DISK_GIB}, e.g. 256").into(),
                ))
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_enable_uefi() -> Result<bool, WizardError> {
    let use_uefi = inquire::Confirm::new("Use UEFI/OVMF firmware?")
        .with_help_message("Recommended — requires edk2-ovmf. Legacy BIOS if declined.")
        .with_default(true)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(use_uefi)
}

fn validate_name(
    s: &str,
) -> Result<inquire::validator::Validation, Box<dyn std::error::Error + Send + Sync>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        Ok(inquire::validator::Validation::Invalid(
            "Name cannot be empty".into(),
        ))
    } else if !is_valid_name(trimmed) {
        Ok(inquire::validator::Validation::Invalid(
            "Name must start with a letter or digit and contain only letters, digits, '_' and '-'"
                .into(),
        ))
    } else {
        Ok(inquire::validator::Validation::Valid)
    }
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn validate_iso_path(path: &str) -> Result<(), WizardError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let has_iso_extension = std::path::Path::new(trimmed)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("iso") || ext.eq_ignore_ascii_case("img"));
    if !has_iso_extension {
        return Err(WizardError::Inquire(format!(
            "Not an ISO image: {trimmed} (expected a .iso or .img file)"
        )));
    }
    if !Path::new(trimmed).exists() {
        return Err(WizardError::Inquire(format!(
            "ISO file not found: {trimmed}"
        )));
    }
    Ok(())
}

fn validate_base_image_path(path: &str) -> Result<(), WizardError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(WizardError::Inquire("Base image path is required".into()));
    }
    if !Path::new(trimmed).exists() {
        return Err(WizardError::Inquire(format!(
            "Base image not found: {trimmed}. Android requires a valid base image."
        )));
    }
    Ok(())
}

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_result_accessors() {
        let linux = BasicResult::Linux(LinuxBasicResult {
            name: "test-vm".into(),
            iso_path: String::new(),
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
            enable_uefi: true,
        });
        assert_eq!(linux.name(), "test-vm");
        assert_eq!(linux.kind(), WizardKind::Linux);
        assert_eq!(linux.disk_size_gib(), 256);
        assert_eq!(linux.instances_root(), "/tmp/instances");
    }

    #[test]
    fn validate_iso_path_empty_is_valid() {
        assert!(validate_iso_path("").is_ok());
        assert!(validate_iso_path("   ").is_ok());
    }

    #[test]
    fn validate_iso_path_missing_file_is_rejected() {
        let err = validate_iso_path("/nonexistent/path/to.iso").unwrap_err();
        assert!(matches!(err, WizardError::Inquire(msg) if msg.contains("ISO file not found")));
    }

    #[test]
    fn validate_iso_path_existing_path_is_valid() {
        let iso = std::env::temp_dir().join("andler-validate-iso-test.iso");
        std::fs::write(&iso, b"").unwrap();
        assert!(validate_iso_path(iso.to_str().unwrap()).is_ok());
        std::fs::remove_file(&iso).ok();
    }

    #[test]
    fn validate_iso_path_without_iso_extension_is_rejected() {
        let dir = std::env::temp_dir();
        let err = validate_iso_path(dir.to_str().unwrap()).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            matches!(&err, WizardError::Inquire(m) if m.contains(".iso")),
            "path without .iso/.img extension must be rejected: {msg}"
        );
    }

    #[test]
    fn validate_name_accepts_safe_names() {
        let valid = validate_name("my-vm_2").unwrap();
        assert_eq!(valid, inquire::validator::Validation::Valid);
    }

    #[test]
    fn validate_name_rejects_spaces_and_special_chars() {
        for bad in [
            "my vm",
            "vm/name",
            "vm\\name",
            "-starts-with-dash",
            ".hidden",
            "ümlaut",
        ] {
            let result = validate_name(bad).unwrap();
            assert!(
                matches!(result, inquire::validator::Validation::Invalid(_)),
                "name {bad:?} must be rejected"
            );
        }
    }
}
