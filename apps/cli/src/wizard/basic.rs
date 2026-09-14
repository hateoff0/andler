use std::path::Path;

use inquire::{Confirm, CustomType, Select, Text};

use crate::helpers::format_bytes;
use crate::{CliAndroidVersion, TracedClient};

use super::advanced::ask_gapps;
use super::ui;
use super::{map_inquire_err, WizardError, WizardKind};

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

#[allow(dead_code)] // kind()/disk_size_gib()/instances_root() are used by the summary and tests
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
    ui::header("linux image & storage");
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

pub async fn run_android(
    client: &mut TracedClient,
    name: String,
    base_image_path: Option<String>,
    instances_root: Option<String>,
) -> Result<AndroidBasicResult, WizardError> {
    ui::header("android image & storage");
    let android_version = ask_android_version()?;
    let gapps = ask_gapps(None)?;
    let choice = super::base_image::ask(client, base_image_path, android_version, gapps).await?;
    let (base_image, base_image_auto_resolved) = (choice.path, choice.auto_resolved);
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
    ui::header("vm type");
    let choice = Select::new("VM type:", vec!["Linux", "Android"])
        .with_help_message(
            "Linux — any distro from an ISO; Android — Waydroid on the base image, \
             with GPU acceleration",
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
    ui::header("name");
    let placeholder = match kind {
        WizardKind::Linux => "my-linux-vm",
        WizardKind::Android => "my-android-vm",
    };
    Text::new("VM name:")
        .with_placeholder(placeholder)
        .with_validator(validate_name)
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

pub fn ask_android_version() -> Result<CliAndroidVersion, WizardError> {
    let choice = Select::new(
        "Android version:",
        vec!["Android 13 (recommended)", "Android 11"],
    )
    .with_help_message("Android 13 tracks Waydroid's current LineageOS builds and has the better Venus/VirtIO-GPU support")
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
    let use_uefi = Confirm::new("Use UEFI/OVMF firmware?")
        .with_help_message("Recommended — requires edk2-ovmf. Legacy BIOS if declined.")
        .with_default(true)
        .prompt()
        .map_err(map_inquire_err)?;
    Ok(use_uefi)
}

fn validate_name(
    s: &str,
) -> Result<inquire::validator::Validation, Box<dyn std::error::Error + Send + Sync>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(inquire::validator::Validation::Invalid(
            "Name cannot be empty".into(),
        ));
    }
    if is_valid_name(s) {
        Ok(inquire::validator::Validation::Valid)
    } else {
        Ok(inquire::validator::Validation::Invalid(
            "Name can contain letters, digits, '-', '_' and '.'".into(),
        ))
    }
}

fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn validate_iso_path(path: &str) -> Result<(), WizardError> {
    if path.is_empty() {
        return Ok(());
    }
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Err(WizardError::Inquire(format!(
            "ISO not found: {path}\n\
             Check the path, or press Enter at the prompt to boot without an ISO."
        )));
    }
    if !path_obj.is_file() {
        return Err(WizardError::Inquire(format!(
            "Not a file: {path} — expected a path to an .iso file."
        )));
    }
    Ok(())
}

pub(crate) fn validate_base_image_path(path: &str) -> Result<(), WizardError> {
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Err(WizardError::Inquire(format!(
            "Base image not found: {path}\n\
             Build one with `docker/images/build.sh <11|13> <VANILLA|GAPPS>`, download a \
             published one with `andler image download`, or point at an existing .qcow2."
        )));
    }
    if !path_obj.is_file() {
        return Err(WizardError::Inquire(format!(
            "Not a file: {path} — expected a path to a .qcow2 base image."
        )));
    }
    Ok(())
}

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
}

/// Human-readable size for the download confirmation; unknown sizes say so
/// instead of printing a made-up number.
pub(crate) fn describe_size(bytes: u64) -> String {
    if bytes == 0 {
        "size unknown".to_string()
    } else {
        format_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_accepts_real_names_and_rejects_separators() {
        assert!(is_valid_name("my-android-vm"));
        assert!(is_valid_name("vm_2.1"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("my vm"));
        assert!(!is_valid_name("vm/2"));
    }

    #[test]
    fn empty_iso_path_is_allowed_and_missing_files_are_not() {
        assert!(validate_iso_path("").is_ok());

        let missing = "/nonexistent/andler-wizard.iso";
        let err = validate_iso_path(missing).unwrap_err();
        assert!(err.to_string().contains("ISO not found"), "{err}");
    }

    #[test]
    fn base_image_validator_points_at_the_build_and_download_commands() {
        let err = validate_base_image_path("/nonexistent/base.qcow2").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("docker/images/build.sh"), "{message}");
        assert!(message.contains("andler image download"), "{message}");
    }

    #[test]
    fn size_description_never_invents_a_number() {
        assert_eq!(describe_size(0), "size unknown");
        assert_eq!(describe_size(2 * 1024 * 1024 * 1024), "2.0GB");
    }
}
