//! Basic wizard questions (VM type, name, ISO/base image, disk size).

use std::path::Path;

use inquire::{CustomType, Select, Text};

use crate::CliAndroidVersion;

use super::{map_inquire_err, WizardError, WizardKind};

const DEFAULT_DISK_GIB: u64 = 256;
const MAX_DISK_GIB: u64 = 65536;

#[derive(Debug, Clone)]
pub struct LinuxBasicResult {
    pub name: String,
    pub iso_path: String,
    pub disk_size_gib: u64,
    pub instances_root: String,
}

#[derive(Debug, Clone)]
pub struct AndroidBasicResult {
    pub name: String,
    pub base_image: String,
    pub android_version: CliAndroidVersion,
    pub disk_size_gib: u64,
    pub instances_root: String,
}

#[derive(Debug, Clone)]
pub enum BasicResult {
    Linux(LinuxBasicResult),
    Android(AndroidBasicResult),
}

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
    Ok(LinuxBasicResult {
        name,
        iso_path: iso,
        disk_size_gib,
        instances_root,
    })
}

pub fn run_android(
    name: String,
    base_image_path: Option<String>,
    instances_root: Option<String>,
) -> Result<AndroidBasicResult, WizardError> {
    let base_image = ask_base_image(base_image_path)?;
    let android_version = ask_android_version()?;
    let disk_size_gib = ask_disk_size(DEFAULT_DISK_GIB)?;
    let instances_root = instances_root.unwrap_or_else(default_instances_root);
    Ok(AndroidBasicResult {
        name,
        base_image,
        android_version,
        disk_size_gib,
        instances_root,
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

pub fn ask_name(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(n) = prefilled {
        return Ok(n);
    }
    Text::new("VM name:")
        .with_placeholder("my-linux-vm")
        .with_validator(|s: &str| validate_name(s))
        .prompt()
        .map_err(map_inquire_err)
}

pub fn ask_iso_path(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        return Ok(p);
    }
    Text::new("Path to ISO image (Enter — skip, boot from disk):")
        .with_placeholder("/home/user/isos/cachyos.iso")
        .with_help_message(
            "Enter path to .iso file, or press Enter to skip (boot from existing disk)",
        )
        .with_default("")
        .prompt()
        .map_err(map_inquire_err)
}

pub fn ask_base_image(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        validate_base_image_path(&p)?;
        return Ok(p);
    }
    let path = Text::new("Path to Android base image:")
        .with_placeholder("/path/to/android-base.qcow2")
        .with_help_message("Path to pre-built Android base image (.qcow2)")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "Base image path is required".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(map_inquire_err)?;
    validate_base_image_path(&path)?;
    Ok(path)
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
        .with_help_message(
            "Thin-provisioned qcow2 — nominal limit, not actual host usage",
        )
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

fn validate_name(
    s: &str,
) -> Result<inquire::validator::Validation, Box<dyn std::error::Error + Send + Sync>> {
    if s.trim().is_empty() {
        Ok(inquire::validator::Validation::Invalid(
            "Name cannot be empty".into(),
        ))
    } else if s.contains('/') || s.contains('\\') {
        Ok(inquire::validator::Validation::Invalid(
            "Name must not contain slashes".into(),
        ))
    } else {
        Ok(inquire::validator::Validation::Valid)
    }
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
        });
        assert_eq!(linux.name(), "test-vm");
        assert_eq!(linux.kind(), WizardKind::Linux);
        assert_eq!(linux.disk_size_gib(), 256);
        assert_eq!(linux.instances_root(), "/tmp/instances");
    }
}
