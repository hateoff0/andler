use crate::helpers::emit_json;
use crate::TracedClient;
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use std::path::PathBuf;

use crate::instance_file::{InstanceFile, InstanceFileResult};
use crate::wizard::{PartialArgs, WizardKind};
use crate::{CliAndroidVersion, CliArmTranslator, CliCdromBus, CliKind};

fn created_json(id: &str) -> serde_json::Value {
    serde_json::json!({ "instance_id": id })
}

#[allow(clippy::too_many_arguments)] // mirrors all create CLI flags; splitting adds indirection for no benefit
pub async fn handle(
    client: &mut TracedClient,
    file: Option<PathBuf>,
    kind: Option<CliKind>,
    name: Option<String>,
    ovmf_vars_template: Option<String>,
    iso_path: Option<String>,
    disk_path: Option<String>,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    no_uefi: bool,
    quick: bool,
    template: Option<String>,
    dry_run: bool,
    verify: bool,
    android_version: Option<CliAndroidVersion>,
    base_image_path: Option<String>,
    gapps: bool,
    microg: bool,
    arm_translator: Option<CliArmTranslator>,
    instances_root: String,
    overlay_size_gib: u64,
    linked_overlay: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_file = file.is_some();
    let has_kind = kind.is_some();

    if has_file && has_kind {
        return Err("--file and --kind are mutually exclusive".into());
    }

    if has_file && quick {
        return Err(
            "--quick and --file are mutually exclusive — --file already provides all configuration"
                .into(),
        );
    }

    if quick && !has_kind {
        return Err("--quick requires --kind to specify VM type".into());
    }

    if dry_run && quick {
        return Err(
            "--dry-run and --quick are mutually exclusive — --quick creates immediately, --dry-run never creates"
                .into(),
        );
    }

    if verify && quick {
        return Err(
            "--verify and --quick are mutually exclusive — --quick creates immediately, --verify never creates"
                .into(),
        );
    }

    if dry_run && verify {
        return Err("--dry-run and --verify are mutually exclusive — pass one or the other".into());
    }

    if has_file {
        let file = file.unwrap();
        let instance_file = InstanceFile::load(&file)?;
        match instance_file.into_result()? {
            InstanceFileResult::Linux(req) => {
                if dry_run {
                    return crate::preview::print_linux_preview(&req);
                }
                if verify {
                    return exit_on_verify_result(crate::verify::verify_linux(&req, json)?);
                }
                let created_name = req.name.clone();
                let response = client.create_instance(req).await?;
                let id = response.into_inner().instance_id;
                println!(
                    "Created instance {created_name} ({})",
                    crate::helpers::short_id(&id)
                );
            }
            InstanceFileResult::Android(req) => {
                if dry_run {
                    return crate::preview::print_android_preview(&req);
                }
                if verify {
                    return exit_on_verify_result(crate::verify::verify_android(&req, json)?);
                }
                let created_name = req.name.clone();
                let response = client.create_android_instance(req).await?;
                let id = response.into_inner().instance_id;
                println!(
                    "Created instance {created_name} ({})",
                    crate::helpers::short_id(&id)
                );
            }
        }
        return Ok(());
    }

    let linux_required = kind == Some(CliKind::Linux)
        && (name.is_none() || iso_path.is_none() || disk_path.is_none());
    let android_required = kind == Some(CliKind::Android) && name.is_none();
    let no_kind = kind.is_none();
    let needs_wizard = quick || no_kind || linux_required || android_required;

    if needs_wizard {
        if dry_run {
            return Err(
                "--dry-run requires --file or all CLI-mode flags for the chosen --kind \
                 (the interactive wizard already shows a full summary before creating, so \
                 --dry-run with a bare `andler create` isn't supported)"
                    .into(),
            );
        }
        if verify {
            return Err(
                "--verify requires --file or all CLI-mode flags for the chosen --kind \
                 (the interactive wizard already shows a full summary before creating, so \
                 --verify with a bare `andler create` isn't supported)"
                    .into(),
            );
        }

        let partial = PartialArgs {
            kind: kind.map(|k| match k {
                CliKind::Linux => WizardKind::Linux,
                CliKind::Android => WizardKind::Android,
            }),
            name: name.clone(),
            iso_path: iso_path.clone(),
            base_image_path: base_image_path.clone(),
            instances_root: Some(instances_root.clone()),
            gapps,
            quick,
            linked: linked_overlay,
        };

        // One code path for both the interactive wizard and `--quick`:
        // resolve the answers, create the VM, then install the guest-side
        // selections the answers imply (ARM translator, clipboard agent).
        return crate::wizard::handle_wizard(client, partial).await;
    }

    let kind = kind.unwrap();
    let name = name.unwrap();
    let ovmf = ovmf_vars_template.unwrap_or_default();
    if template.is_some() && kind == CliKind::Android {
        return Err(
            "--template is only supported with --kind linux in this phase (Android requests              carry no config sections yet)"
                .into(),
        );
    }
    let template = match &template {
        Some(name) => Some(crate::instance_file::TemplateFile::load(name)?),
        None => None,
    };

    match kind {
        CliKind::Linux => {
            let iso = iso_path.ok_or("--iso-path is required for --kind linux")?;
            let disk = disk_path.ok_or("--disk-path is required for --kind linux")?;

            let (iso, disk) = match validate_linux_paths(&iso, &disk) {
                Ok(paths) => paths,
                Err(msg) => return Err(msg.into()),
            };

            let req = build_linux_request(
                name.clone(),
                iso,
                disk,
                disk_size_gib,
                compact_on_shutdown,
                cdrom_bus,
                ovmf,
                !no_uefi,
                template.as_ref(),
            );
            if dry_run {
                if json {
                    let resolved = crate::preview::resolve_linux(&req)?;
                    emit_json(&resolved.cfg)?;
                } else {
                    crate::preview::print_linux_preview(&req)?;
                }
                return Ok(());
            }
            if verify {
                return exit_on_verify_result(crate::verify::verify_linux(&req, json)?);
            }
            let response = client.create_instance(req).await?;
            let id = response.into_inner().instance_id;
            if json {
                emit_json(&created_json(&id))?;
            } else {
                println!(
                    "Created instance {name} ({})",
                    crate::helpers::short_id(&id)
                );
            }
        }
        CliKind::Android => {
            let av = android_version.ok_or("--android-version is required for --kind android")?;
            let arm_translator = arm_translator.unwrap_or(CliArmTranslator::None);
            let bip = match base_image_path {
                Some(bip) => match validate_base_image_path(&bip) {
                    Ok(path) => path,
                    Err(msg) => return Err(msg.into()),
                },
                None => String::new(),
            };

            let req = build_android_request(
                name.clone(),
                av,
                bip,
                ovmf,
                gapps,
                microg,
                arm_translator,
                instances_root,
                overlay_size_gib,
                linked_overlay,
            );
            if dry_run {
                if json {
                    let resolved = crate::preview::resolve_android(&req)?;
                    emit_json(&resolved.cfg)?;
                } else {
                    crate::preview::print_android_preview(&req)?;
                }
                return Ok(());
            }
            if verify {
                return exit_on_verify_result(crate::verify::verify_android(&req, json)?);
            }
            let response = client.create_android_instance(req).await?;
            let id = response.into_inner().instance_id;
            if json {
                emit_json(&created_json(&id))?;
            } else {
                println!(
                    "Created instance {name} ({})",
                    crate::helpers::short_id(&id)
                );
            }
        }
    }

    Ok(())
}

fn exit_on_verify_result(all_passed: bool) -> Result<(), Box<dyn std::error::Error>> {
    if all_passed {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

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

fn validate_base_image_path(base_image_path: &str) -> Result<String, String> {
    let canonical_base_image = std::fs::canonicalize(base_image_path)
        .map_err(|e| format!("base image not found: {base_image_path} ({e})"))?
        .to_string_lossy()
        .into_owned();

    Ok(canonical_base_image)
}

#[allow(clippy::too_many_arguments)] // mirrors the create CLI flags for Linux VMs
fn build_linux_request(
    name: String,
    iso_path: String,
    disk_path: String,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    ovmf_vars_template: String,
    enable_uefi: bool,
    template: Option<&crate::instance_file::TemplateFile>,
) -> CreateInstanceRequest {
    let mut disk = andler_core::DiskConfig::reference_default(std::path::PathBuf::from(&disk_path));
    if let Some(gib) = disk_size_gib {
        disk.size_bytes = gib
            .checked_mul(andler_core::DiskConfig::GIB)
            .expect("disk size overflow");
    }
    disk.compact_on_shutdown = compact_on_shutdown;

    let resolved_cdrom_bus = match cdrom_bus {
        CliCdromBus::Auto => {
            andler_core::CdromBus::recommended_for_iso_filename(std::path::Path::new(&iso_path))
        }
        CliCdromBus::Virtio => andler_core::CdromBus::VirtioScsi,
        CliCdromBus::Ide => andler_core::CdromBus::Ide,
    };

    // Template sections sit between the reference defaults and the CLI
    // flags: sections not present in the template keep their defaults, and
    // sections are only settable through a template, so no flag overwrite
    // is needed here.
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
                enable_uefi,
                ovmf_code_path: std::path::PathBuf::new(),
                ovmf_vars_path: std::path::PathBuf::from(&ovmf_vars_template),
            }
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
        ..Default::default()
    };
    if let Some(t) = template {
        if let Some(cpu) = &t.cpu {
            req.cpu = Some(cpu.clone().into());
        }
        if let Some(memory) = &t.memory {
            req.memory = Some(memory.clone().into());
        }
        if let Some(display) = &t.display {
            req.display = Some((*display).into());
        }
        if let Some(gpu) = &t.gpu {
            req.gpu = Some(gpu.clone().into());
        }
        if let Some(network) = &t.network {
            req.network = Some(network.clone().into());
        }
        if let Some(audio) = &t.audio {
            req.audio = Some((*audio).into());
        }
        if let Some(input) = &t.input {
            req.input = Some((*input).into());
        }
    }
    req.set_cdrom_bus(resolved_cdrom_bus.into());
    req
}

#[allow(clippy::too_many_arguments)] // mirrors the create CLI flags for Android VMs
fn build_android_request(
    name: String,
    android_version: CliAndroidVersion,
    base_image_path: String,
    ovmf_vars_template: String,
    gapps: bool,
    microg: bool,
    arm_translator: CliArmTranslator,
    instances_root: String,
    overlay_size_gib: u64,
    linked_overlay: bool,
) -> CreateAndroidInstanceRequest {
    let mut profile = ProtoAndroidProfile {
        gapps,
        microg,
        ..Default::default()
    };
    profile.set_android_version(android_version.into());
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
        linked_overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_linux_paths_empty_iso_is_allowed() {
        assert!(validate_linux_paths("", "/tmp").is_ok());
    }

    #[test]
    fn validate_linux_paths_missing_iso_is_rejected() {
        let err = validate_linux_paths("/nonexistent/path/to.iso", "/tmp").unwrap_err();
        assert!(err.contains("ISO file not found"));
    }

    #[test]
    fn validate_linux_paths_missing_disk_directory_is_rejected() {
        let err = validate_linux_paths("", "/nonexistent/andler-test-dir/disk.qcow2").unwrap_err();
        assert!(err.contains("disk directory does not exist"));
    }

    #[test]
    fn validate_linux_paths_bare_relative_disk_filename_is_allowed() {
        assert!(validate_linux_paths("", "disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_existing_disk_directory_is_allowed() {
        assert!(validate_linux_paths("", "/tmp/disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_canonicalizes_disk_directory() {
        let (_, disk) = validate_linux_paths("", "/tmp/../tmp/my-disk.qcow2").unwrap();
        assert!(!disk.contains(".."));
        assert!(disk.ends_with("my-disk.qcow2"));
    }

    #[test]
    fn validate_linux_paths_canonicalizes_iso_path() {
        let (iso, _) = validate_linux_paths("/tmp/../tmp", "disk.qcow2").unwrap();
        assert!(!iso.contains(".."));
    }

    #[test]
    fn validate_base_image_path_missing_is_rejected() {
        let err = validate_base_image_path("/nonexistent/base.qcow2").unwrap_err();
        assert!(err.contains("base image not found"));
    }

    #[test]
    fn validate_base_image_path_existing_is_allowed() {
        assert!(validate_base_image_path("/tmp").is_ok());
    }

    #[test]
    fn created_json_serializes_instance_id() {
        let json = created_json("0123456789abcdef0123456789abcdef");
        assert_eq!(json["instance_id"], "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn validate_base_image_path_canonicalizes() {
        let base_image = validate_base_image_path("/tmp/../tmp").unwrap();
        assert!(!base_image.contains(".."));
    }
}
