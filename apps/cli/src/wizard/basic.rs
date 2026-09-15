use std::path::Path;

use crate::helpers::format_bytes;
use crate::{CliAndroidVersion, TracedClient};

use super::advanced::ask_gapps;
use super::ui;
use super::{WizardError, WizardKind};

const DEFAULT_DISK_GIB: u64 = 256;
const MAX_DISK_GIB: u64 = 65536;

#[derive(Debug, Clone)]
pub struct LinuxBasicResult {
    pub name: String,
    pub iso_path: String,
    pub disk_size_gib: u64,
    pub instances_root: String,
    /// `Some` when the flow asked (or a flag answered); `None` leaves the
    /// choice to the resolver's host-firmware default.
    pub enable_uefi: Option<bool>,
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
    disk_size_gib: Option<u64>,
) -> Result<LinuxBasicResult, WizardError> {
    ui::step("linux image & storage")?;
    let iso = ask_iso_path(iso_path)?;
    let disk_size_gib = ask_disk_size(disk_size_gib.unwrap_or(DEFAULT_DISK_GIB))?;
    let instances_root = instances_root.unwrap_or_else(crate::create::default_instances_root);
    let enable_uefi = Some(ask_enable_uefi()?);
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
    disk_size_gib: Option<u64>,
) -> Result<AndroidBasicResult, WizardError> {
    ui::step("android image & storage")?;
    let android_version = ask_android_version()?;
    let gapps = ask_gapps(None)?;
    let choice = super::base_image::ask(client, base_image_path, android_version, gapps).await?;
    let (base_image, base_image_auto_resolved) = (choice.path, choice.auto_resolved);
    let disk_size_gib = ask_disk_size(disk_size_gib.unwrap_or(DEFAULT_DISK_GIB))?;
    let instances_root = instances_root.unwrap_or_else(crate::create::default_instances_root);
    Ok(AndroidBasicResult {
        name,
        base_image,
        base_image_auto_resolved,
        android_version,
        gapps,
        disk_size_gib,
        instances_root,
    })
}

pub fn ask_kind(prefilled: Option<WizardKind>) -> Result<WizardKind, WizardError> {
    if let Some(k) = prefilled {
        return Ok(k);
    }
    let kind = cliclack::select("which VM do you want to create?")
        .item(WizardKind::Linux, "Linux", "any distro from an ISO")
        .item(
            WizardKind::Android,
            "Android",
            "Waydroid on a base image, with GPU acceleration",
        )
        .initial_value(WizardKind::Linux)
        .interact()?;

    Ok(kind)
}

pub fn ask_name(prefilled: Option<String>, kind: WizardKind) -> Result<String, WizardError> {
    if let Some(n) = prefilled {
        return Ok(n);
    }
    let placeholder = match kind {
        WizardKind::Linux => "my-linux-vm",
        WizardKind::Android => "my-android-vm",
    };
    let name: String = cliclack::input("what should the VM be called?")
        .placeholder(placeholder)
        // cliclack hands a validator a `&String`; the check itself takes the
        // slice it actually needs.
        .validate(|input: &String| validate_name(input))
        .interact()?;
    // The name reaches `instance.toml` and the daemon's config validation:
    // trimming it here is what the operator meant, and it keeps a stray space
    // from failing that validation later.
    Ok(name.trim().to_string())
}

pub fn ask_iso_path(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        validate_iso_path(&p)?;
        return Ok(p);
    }
    let path: String =
        cliclack::input("path to the ISO image\nEnter skips it and boots from the disk")
            .placeholder("/home/user/isos/cachyos.iso")
            .required(false)
            .validate(|input: &String| validate_iso_input(input))
            .interact()?;
    let path = path.trim().to_string();
    validate_iso_path(&path)?;
    Ok(path)
}

pub fn ask_android_version() -> Result<CliAndroidVersion, WizardError> {
    let version = cliclack::select("android version")
        .item(
            CliAndroidVersion::Android13,
            "Android 13",
            "recommended — newest LineageOS base",
        )
        .item(
            CliAndroidVersion::Android11,
            "Android 11",
            "previous LineageOS base",
        )
        .initial_value(CliAndroidVersion::Android13)
        .interact()?;

    Ok(version)
}

pub fn ask_disk_size(default_gib: u64) -> Result<u64, WizardError> {
    let size: u64 = cliclack::input(
        "disk size (GiB)\nthin-provisioned qcow2 — a nominal limit, not host usage",
    )
    .default_input(&default_gib.to_string())
    .validate(|input: &String| validate_disk_size(input))
    .interact()?;

    Ok(size)
}

fn ask_enable_uefi() -> Result<bool, WizardError> {
    let use_uefi = cliclack::confirm(
        "use UEFI/OVMF firmware?\nrecommended — needs edk2-ovmf; No uses legacy BIOS",
    )
    .initial_value(true)
    .interact()?;
    Ok(use_uefi)
}

fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the name cannot be empty".into());
    }
    if is_valid_name(name) {
        Ok(())
    } else {
        Err("name can contain letters, digits, '-', '_' and '.'".into())
    }
}

fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn validate_disk_size(input: &str) -> Result<(), String> {
    let range = format!("enter an integer between 1 and {MAX_DISK_GIB}, e.g. 256");
    let gib: u64 = input.trim().parse().map_err(|_| range.clone())?;
    if (1..=MAX_DISK_GIB).contains(&gib) {
        Ok(())
    } else {
        Err(range)
    }
}

fn validate_iso_input(input: &str) -> Result<(), String> {
    validate_iso_path(input.trim()).map_err(|e| e.to_string())
}

fn validate_iso_path(path: &str) -> Result<(), WizardError> {
    if path.is_empty() {
        return Ok(());
    }
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Err(WizardError::Message(format!(
            "ISO not found: {path}\n\
             Check the path, or press Enter at the prompt to boot without an ISO."
        )));
    }
    if !path_obj.is_file() {
        return Err(WizardError::Message(format!(
            "Not a file: {path} — expected a path to an .iso file."
        )));
    }
    Ok(())
}

pub(crate) fn validate_base_image_path(path: &str) -> Result<(), WizardError> {
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Err(WizardError::Message(format!(
            "Base image not found: {path}\n\
             Build one with `docker/images/build.sh <11|13> <VANILLA|GAPPS>`, download a \
             published one with `andler image download`, or point at an existing .qcow2."
        )));
    }
    if !path_obj.is_file() {
        return Err(WizardError::Message(format!(
            "Not a file: {path} — expected a path to a .qcow2 base image."
        )));
    }
    Ok(())
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
        assert!(validate_name("my-android-vm").is_ok());
        assert!(validate_name("vm_2.1").is_ok());
        assert!(
            validate_name(" my-linux ").is_ok(),
            "the prompt trims what it returns, so surrounding space is not a rejection"
        );
        assert!(validate_name("").is_err());
        assert!(validate_name("my vm").is_err());
        assert!(validate_name("vm/2").is_err());
    }

    #[test]
    fn disk_size_validation_holds_the_documented_range() {
        assert!(validate_disk_size("1").is_ok());
        assert!(validate_disk_size("256").is_ok());
        assert!(validate_disk_size("65536").is_ok());
        assert!(validate_disk_size(" 512 ").is_ok());
        assert!(validate_disk_size("0").is_err());
        assert!(validate_disk_size("65537").is_err());
        assert!(validate_disk_size("16 GiB").is_err());
        assert!(
            validate_disk_size("0").unwrap_err().contains("1 and 65536"),
            "the error must name the range, not just the failure"
        );
    }

    #[test]
    fn empty_iso_path_is_allowed_and_missing_files_are_not() {
        assert!(validate_iso_path("").is_ok());
        assert!(validate_iso_input("").is_ok());

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
