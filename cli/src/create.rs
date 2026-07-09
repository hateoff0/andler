use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use std::path::PathBuf;
use tonic::transport::Channel;

use crate::instance_file::{InstanceFile, InstanceFileResult};
use crate::wizard::{PartialArgs, WizardError, WizardKind};
use crate::{err_exit, CliAndroidVersion, CliArmTranslator, CliCdromBus, CliKind, CliRootMode};

#[allow(clippy::too_many_arguments)]
pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    file: Option<PathBuf>,
    kind: Option<CliKind>,
    name: Option<String>,
    ovmf_vars_template: Option<String>,
    iso_path: Option<String>,
    disk_path: Option<String>,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    quick: bool,
    android_version: Option<CliAndroidVersion>,
    base_image_path: Option<String>,
    gapps: bool,
    microg: bool,
    arm_translator: Option<CliArmTranslator>,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_file = file.is_some();
    let has_kind = kind.is_some();

    if has_file && has_kind {
        err_exit("error: --file and --kind are mutually exclusive");
    }

    if has_file && quick {
        err_exit("error: --quick and --file are mutually exclusive — --file already provides all configuration");
    }

    if quick && !has_kind {
        err_exit("error: --quick requires --kind to specify VM type");
    }

    // --- TOML mode ---
    if has_file {
        let file = file.unwrap();
        let instance_file = InstanceFile::load(&file)?;
        match instance_file.into_result() {
            InstanceFileResult::Linux(req) => {
                let response = client.create_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
            InstanceFileResult::Android(req) => {
                let response = client.create_android_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
        }
        return Ok(());
    }

    // --- Wizard mode: when required CLI parameters are missing, or --quick ---
    let linux_required = kind == Some(CliKind::Linux)
        && (name.is_none() || iso_path.is_none() || disk_path.is_none());
    let android_required = kind == Some(CliKind::Android)
        && (name.is_none() || base_image_path.is_none());
    let no_kind = kind.is_none();
    let needs_wizard = quick || no_kind || linux_required || android_required;

    if needs_wizard {
        let partial = PartialArgs {
            kind: kind.map(|k| match k {
                CliKind::Linux => WizardKind::Linux,
                CliKind::Android => WizardKind::Android,
            }),
            name: name.clone(),
            iso_path: iso_path.clone(),
            base_image_path: base_image_path.clone(),
            instances_root: Some(instances_root.clone()),
            quick,
        };

        let result = crate::wizard::run(partial).await;

        return match result {
            Ok(result) => crate::wizard::send_result(client, result).await,
            Err(WizardError::Cancelled) => {
                println!("Cancelled.");
                Ok(())
            }
            Err(WizardError::NotTty) => {
                eprintln!("{}", WizardError::NotTty);
                std::process::exit(1);
            }
            Err(e) => Err(e.into()),
        };
    }

    // --- CLI mode (all required parameters provided) ---
    let kind = kind.unwrap();
    let name = name.unwrap();
    let ovmf = ovmf_vars_template.unwrap_or_default();

    match kind {
        CliKind::Linux => {
            let iso = iso_path
                .unwrap_or_else(|| err_exit("error: --iso-path is required for --kind linux"));
            let disk = disk_path
                .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));

            let (iso, disk) = match validate_linux_paths(&iso, &disk) {
                Ok(paths) => paths,
                Err(msg) => err_exit(&format!("error: {msg}")),
            };

            let req = build_linux_request(
                name,
                iso,
                disk,
                disk_size_gib,
                compact_on_shutdown,
                cdrom_bus,
                ovmf,
            );
            let response = client.create_instance(req).await?;
            println!("{}", response.into_inner().instance_id);
        }
        CliKind::Android => {
            let av = android_version.unwrap_or_else(|| {
                err_exit("error: --android-version is required for --kind android")
            });
            let bip = base_image_path.unwrap_or_else(|| {
                err_exit("error: --base-image-path is required for --kind android")
            });

            if root == CliRootMode::Magisk && magisk_dir.is_none() {
                err_exit("error: --magisk-dir is required when --root magisk");
            }

            let (bip, canonical_magisk_dir) =
                match validate_android_paths(&bip, magisk_dir.as_deref()) {
                    Ok(paths) => paths,
                    Err(msg) => err_exit(&format!("error: {msg}")),
                };
            let magisk_dir = canonical_magisk_dir.map(PathBuf::from);

            let arm_translator = arm_translator.unwrap_or(CliArmTranslator::None);

            let req = build_android_request(
                name,
                av,
                bip,
                ovmf,
                gapps,
                microg,
                arm_translator,
                root,
                instances_root,
                overlay_size_gib,
                magisk_dir,
            );
            let response = client.create_android_instance(req).await?;
            println!("{}", response.into_inner().instance_id);
        }
    }

    Ok(())
}

/// Pre-flight checks for `andler create --kind linux` (direct CLI
/// flags, not the wizard or `--file` paths — the wizard already
/// validates interactively as the person types, see
/// `wizard::basic::ask_iso_path`/`ask_base_image`, and `--file` is the
/// user's own hand-written TOML, validated by `InstanceFile::load`).
/// Catches obviously-wrong paths before spending a round-trip to
/// `andlerd` on a request that's guaranteed to fail once the backend
/// actually tries to use them — see PLAN.md, item 15, "Config
/// validation before creation".
///
/// Also canonicalizes both paths (resolves `..`/`.`/symlinks to an
/// absolute path) and returns the canonical forms — see PLAN.md, item
/// 20c, "No path canonicalization": a relative or symlink-containing
/// path would otherwise be sent to `andlerd` as-is and only resolved
/// there, at whatever point it's actually opened, which is later and
/// further from where the person's input was accepted. `disk_path`
/// itself usually doesn't exist yet (this is the common "create a new
/// disk" case, not "point at an existing one") — `canonicalize` requires
/// the full path to exist, so only its *parent* is canonicalized and
/// the (not-yet-existing) file name is reattached, rather than trying
/// and failing to canonicalize the whole thing.
///
/// Deliberately narrow: only existence + canonicalization on paths the
/// CLI itself already has in hand, not a re-implementation of every
/// validation the daemon/backend will do anyway (disk size limits,
/// resolution format, etc.) — duplicating those here would just be two
/// places to keep in sync for no real benefit, since the daemon has to
/// validate them regardless of what the CLI checked first.
fn validate_linux_paths(iso_path: &str, disk_path: &str) -> Result<(String, String), String> {
    let canonical_iso = if iso_path.is_empty() {
        String::new()
    } else {
        let canonical = std::fs::canonicalize(iso_path)
            .map_err(|e| format!("ISO file not found: {iso_path} ({e})"))?;
        canonical.to_string_lossy().into_owned()
    };

    let disk = std::path::Path::new(disk_path);
    let canonical_disk = match disk.parent() {
        // An empty parent (e.g. a bare relative filename like
        // "disk.qcow2") means "current directory" — nothing to
        // canonicalize against, and always "exists" in the relevant
        // sense (`Path::new("").exists()` would actually return `false`
        // since `""` isn't a valid path to stat, so this must be
        // checked explicitly).
        Some(parent) if !parent.as_os_str().is_empty() => {
            let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
                format!("disk directory does not exist: {} ({e})", parent.display())
            })?;
            let file_name = disk
                .file_name()
                .ok_or_else(|| format!("invalid disk path: {disk_path}"))?;
            canonical_parent
                .join(file_name)
                .to_string_lossy()
                .into_owned()
        }
        _ => disk_path.to_string(),
    };

    Ok((canonical_iso, canonical_disk))
}

/// Pre-flight checks for `andler create --kind android` (direct CLI
/// flags) — see `validate_linux_paths`'s doc comment for why this is
/// narrow and why the wizard/`--file` paths aren't covered here too.
/// Both `base_image_path` and `magisk_dir` (if given) are required to
/// already exist, so — unlike `disk_path` in `validate_linux_paths` —
/// both can be canonicalized directly, no "doesn't exist yet" case to
/// special-case. Returns `(canonical_base_image_path,
/// canonical_magisk_dir)`.
fn validate_android_paths(
    base_image_path: &str,
    magisk_dir: Option<&std::path::Path>,
) -> Result<(String, Option<String>), String> {
    let canonical_base_image = std::fs::canonicalize(base_image_path)
        .map_err(|e| format!("base image not found: {base_image_path} ({e})"))?
        .to_string_lossy()
        .into_owned();

    let canonical_magisk_dir = match magisk_dir {
        Some(dir) => Some(
            std::fs::canonicalize(dir)
                .map_err(|e| {
                    format!("Magisk directory does not exist: {} ({e})", dir.display())
                })?
                .to_string_lossy()
                .into_owned(),
        ),
        None => None,
    };

    Ok((canonical_base_image, canonical_magisk_dir))
}

fn build_linux_request(
    name: String,
    iso_path: String,
    disk_path: String,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    ovmf_vars_template: String,
) -> CreateInstanceRequest {
    let mut disk = andler_core::DiskConfig::reference_default(std::path::PathBuf::from(&disk_path));
    if let Some(gib) = disk_size_gib {
        disk.size_bytes = gib
            .checked_mul(andler_core::DiskConfig::GIB)
            .expect("disk size overflow");
    }
    disk.compact_on_shutdown = compact_on_shutdown;

    let resolved_cdrom_bus = match cdrom_bus {
        CliCdromBus::Auto => andler_core::CdromBus::recommended_for_iso_filename(
            std::path::Path::new(&iso_path),
        ),
        CliCdromBus::Virtio => andler_core::CdromBus::VirtioScsi,
        CliCdromBus::Ide => andler_core::CdromBus::Ide,
    };

    let mut req = CreateInstanceRequest {
        name,
        iso_path,
        cpu: Some(andler_core::CpuConfig::reference_default().into()),
        memory: Some(andler_core::MemoryConfig::reference_default().into()),
        disk: Some(disk.into()),
        display: Some(andler_core::DisplayConfig::reference_default().into()),
        gpu: Some(andler_core::GpuConfig::reference_default().into()),
        network: Some(andler_core::NetworkConfig::reference_default().into()),
        firmware: Some(
            andler_core::FirmwareConfig {
                ovmf_code_path: std::path::PathBuf::new(),
                ovmf_vars_path: std::path::PathBuf::from(&ovmf_vars_template),
            }
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
        ..Default::default()
    };
    req.set_cdrom_bus(resolved_cdrom_bus.into());
    req
}

fn build_android_request(
    name: String,
    android_version: CliAndroidVersion,
    base_image_path: String,
    ovmf_vars_template: String,
    gapps: bool,
    microg: bool,
    arm_translator: CliArmTranslator,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> CreateAndroidInstanceRequest {
    let mut profile = ProtoAndroidProfile {
        gapps,
        microg,
        ..Default::default()
    };
    profile.set_android_version(android_version.into());
    profile.set_root(root.into());
    profile.set_arm_translator(arm_translator.into());

    CreateAndroidInstanceRequest {
        name,
        profile: Some(profile),
        base_image_path,
        instances_root,
        overlay_size_bytes: overlay_size_gib
            .checked_mul(1024 * 1024 * 1024)
            .expect("overlay size overflow"),
        ovmf_vars_template,
        magisk_dir: magisk_dir
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_linux_paths_empty_iso_is_allowed() {
        // Empty ISO path means "boot from existing disk" — not an error.
        assert!(validate_linux_paths("", "/tmp").is_ok());
    }

    #[test]
    fn validate_linux_paths_missing_iso_is_rejected() {
        let err = validate_linux_paths("/nonexistent/path/to.iso", "/tmp").unwrap_err();
        assert!(err.contains("ISO file not found"));
    }

    #[test]
    fn validate_linux_paths_missing_disk_directory_is_rejected() {
        let err =
            validate_linux_paths("", "/nonexistent/andler-test-dir/disk.qcow2").unwrap_err();
        assert!(err.contains("disk directory does not exist"));
    }

    #[test]
    fn validate_linux_paths_bare_relative_disk_filename_is_allowed() {
        // No parent component at all (current directory) — must not be
        // treated as "parent doesn't exist".
        assert!(validate_linux_paths("", "disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_existing_disk_directory_is_allowed() {
        assert!(validate_linux_paths("", "/tmp/disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_canonicalizes_disk_directory() {
        // "/tmp/../tmp/disk.qcow2" and "/tmp/disk.qcow2" must resolve to
        // the same canonical path — the whole point of item 20c is that
        // a `..`-containing path doesn't reach `andlerd` as-is.
        let (_, disk) = validate_linux_paths("", "/tmp/../tmp/my-disk.qcow2").unwrap();
        assert!(!disk.contains(".."));
        assert!(disk.ends_with("my-disk.qcow2"));
    }

    #[test]
    fn validate_linux_paths_canonicalizes_iso_path() {
        // Reuse /tmp itself as a stand-in "ISO" — canonicalize only
        // cares that the path exists and resolves it, doesn't care
        // whether it's actually an ISO file.
        let (iso, _) = validate_linux_paths("/tmp/../tmp", "disk.qcow2").unwrap();
        assert!(!iso.contains(".."));
    }

    #[test]
    fn validate_android_paths_missing_base_image_is_rejected() {
        let err = validate_android_paths("/nonexistent/base.qcow2", None).unwrap_err();
        assert!(err.contains("base image not found"));
    }

    #[test]
    fn validate_android_paths_missing_magisk_dir_is_rejected() {
        let dir = std::path::Path::new("/nonexistent/magisk-dir");
        let err = validate_android_paths("/tmp", Some(dir)).unwrap_err();
        assert!(err.contains("Magisk directory does not exist"));
    }

    #[test]
    fn validate_android_paths_existing_paths_are_allowed() {
        assert!(validate_android_paths("/tmp", Some(std::path::Path::new("/tmp"))).is_ok());
    }

    #[test]
    fn validate_android_paths_canonicalizes_base_image() {
        let (base_image, _) = validate_android_paths("/tmp/../tmp", None).unwrap();
        assert!(!base_image.contains(".."));
    }
}
