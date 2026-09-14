use std::path::PathBuf;

use andler_core::config::HostFirmware;
use andler_core::{DiskFormat, InstanceConfig, InstanceKind};

use crate::create::Creation;

/// A creation as the client-side commands (`--dry-run`, `--verify`) see it:
/// the resolved config plus where it would land on disk, the OVMF template it
/// would provision, and the validation verdict of the one resolver.
pub(crate) struct Resolved {
    pub cfg: InstanceConfig,
    pub instance_dir: PathBuf,
    pub ovmf_vars_template: Option<PathBuf>,
    pub android_base_image: Option<String>,
    pub validation: Result<(), String>,
}

impl Resolved {
    pub(crate) fn from_creation(creation: &Creation, host: &HostFirmware) -> Self {
        let mut cfg = creation.cfg.clone();
        let instances_root = if creation.instances_root.is_empty() {
            andler_core::paths::instances_root()
        } else {
            PathBuf::from(&creation.instances_root)
        };
        let instance_dir = instances_root.join(cfg.id.to_string());

        let explicit_vars = cfg.firmware.ovmf_vars_path.clone();
        let template = if explicit_vars.as_os_str().is_empty() {
            host.ovmf_vars_template.clone()
        } else {
            Some(explicit_vars)
        };
        let ovmf_vars_template = if cfg.firmware.enable_uefi {
            template
        } else {
            None
        };

        if cfg.firmware.enable_uefi {
            if cfg.firmware.ovmf_code_path.as_os_str().is_empty() {
                if let Some(code) = &host.ovmf_code_path {
                    cfg.firmware.ovmf_code_path = code.clone();
                }
            }
            if ovmf_vars_template.is_some() {
                cfg.firmware.ovmf_vars_path = instance_dir.join("VARS.fd");
            }
        }

        match &cfg.kind {
            InstanceKind::LinuxVm { .. } => relocate_fresh_disk(&mut cfg, &instance_dir),
            InstanceKind::AndroidVm { .. } => cfg.disk.path = instance_dir.join("disk.qcow2"),
        }

        let validation = cfg.validate();
        let android_base_image =
            (!creation.base_image_path.is_empty()).then(|| creation.base_image_path.clone());

        Resolved {
            cfg,
            instance_dir,
            ovmf_vars_template,
            android_base_image,
            validation,
        }
    }
}

/// Reports what `--dry-run` would create, refusing a config the daemon would
/// reject: the verdict is the one the resolver already computed, so dry-run
/// and create cannot disagree about whether the input is creatable.
pub(crate) fn report_dry_run(
    resolved: &Resolved,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(issue) = &resolved.validation {
        return Err(format!("resolved configuration is not creatable: {issue}").into());
    }
    if json {
        crate::helpers::emit_json(&resolved.cfg)?;
        return Ok(());
    }
    print_preview(resolved)
}

fn relocate_fresh_disk(cfg: &mut InstanceConfig, instance_dir: &std::path::Path) {
    if cfg.disk.format == DiskFormat::Qcow2 && !cfg.disk.path.exists() {
        let disk_file_name = cfg
            .disk
            .path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("disk.qcow2"));
        cfg.disk.path = instance_dir.join(disk_file_name);
    }
}

fn print_preview(resolved: &Resolved) -> Result<(), Box<dyn std::error::Error>> {
    let Resolved {
        cfg,
        instance_dir,
        ovmf_vars_template,
        ..
    } = resolved;
    let ovmf_vars_template = ovmf_vars_template.as_deref();

    println!("─── Dry run: this would create ───────────────────────────────");
    println!("Name:            {}", cfg.name);

    match &cfg.kind {
        InstanceKind::LinuxVm {
            iso_path,
            cdrom_bus,
        } => {
            println!("Type:            Linux VM");
            if iso_path.as_os_str().is_empty() {
                println!("ISO:             (none — boots from disk)");
            } else {
                println!(
                    "ISO:             {} (bus: {cdrom_bus:?})",
                    iso_path.display()
                );
            }
        }
        InstanceKind::AndroidVm { android_profile } => {
            println!(
                "Type:            Android VM ({:?})",
                android_profile.android_version
            );
            println!("GApps:           {}", android_profile.gapps);
            println!("MicroG:          {}", android_profile.microg);
            println!("ARM translator:  {}", android_profile.arm_translator);
        }
    }

    println!(
        "Instance dir:    {} (id is a placeholder — the real one is assigned at creation time)",
        instance_dir.display()
    );
    println!(
        "Disk:            {} ({} GiB, {:?})",
        cfg.disk.path.display(),
        cfg.disk.size_bytes / andler_core::DiskConfig::GIB,
        cfg.disk.format
    );
    if let InstanceKind::AndroidVm { .. } = &cfg.kind {
        match &cfg.disk.base_image {
            Some(base) => println!(
                "Disk mode:       linked overlay (backing file: {})",
                base.display()
            ),
            None => println!("Disk mode:       full copy (independent of base image)"),
        }
    }
    println!("CPU:             {} cores", cfg.cpu.cores);
    println!(
        "Memory:          {} GiB",
        cfg.memory.size_bytes / andler_core::MemoryConfig::GIB
    );
    println!(
        "GPU:             {:?}, {} MiB",
        cfg.gpu.render_backend,
        cfg.gpu.hostmem_bytes / andler_core::GpuConfig::MIB
    );
    println!(
        "Display:         {}x{}, {:?}{}",
        cfg.display.resolution.width,
        cfg.display.resolution.height,
        cfg.display.display_engine,
        if cfg.display.fullscreen {
            ", fullscreen"
        } else {
            ""
        }
    );
    println!("Audio:           {:?}", cfg.audio.backend);
    println!(
        "Network:         {:?} (NAT backend: {:?})",
        cfg.network.mode, cfg.network.nat_backend
    );
    match ovmf_vars_template {
        Some(t) => println!(
            "OVMF VARS:       {} (template — copied into instance dir at creation time)",
            t.display()
        ),
        None => println!("OVMF:            not found — Legacy BIOS will be used"),
    }
    println!();

    let qmp_placeholder = instance_dir.join("qmp.sock");
    let args = andler_qemu::cmdline::build_args(cfg, &qmp_placeholder)?;
    println!("Resolved QEMU command line:");
    println!("  qemu-system-x86_64 \\");
    for chunk in args.chunks(2) {
        match chunk {
            [flag, value] => println!("    {flag} {value} \\"),
            [flag] => println!("    {flag} \\"),
            _ => unreachable!(),
        }
    }
    println!();
    println!("Nothing was created. Remove --dry-run to actually create this VM.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::Creation;
    use andler_core::config::{ConfigDraft, DiskDraft, DiskSource, DraftKind, FirmwareDraft};

    fn draft(disk_path: &str) -> ConfigDraft {
        ConfigDraft {
            name: "preview-vm".to_string(),
            kind: DraftKind::Linux {
                iso_path: PathBuf::from("/isos/ubuntu-24.04.iso"),
                cdrom_bus: andler_core::CdromBus::VirtioScsi,
            },
            disk: DiskDraft {
                path: PathBuf::from(disk_path),
                source: DiskSource::Fresh,
                size_gib: Some(64),
                compact_on_shutdown: false,
                snapshot_timeout_secs: None,
            },
            firmware: FirmwareDraft::default(),
            autostart: false,
            cpu: None,
            memory: None,
            display: None,
            gpu: None,
            network: None,
            audio: None,
            input: None,
        }
    }

    fn host() -> HostFirmware {
        HostFirmware {
            ovmf_code_path: Some(PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")),
            ovmf_vars_template: Some(PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        }
    }

    fn resolve(disk_path: &str) -> Resolved {
        let creation = Creation::resolve(&draft(disk_path), "/tmp/instances".to_string(), &host())
            .expect("resolves");
        Resolved::from_creation(&creation, &host())
    }

    #[test]
    fn detected_firmware_reaches_the_preview_command_line() {
        let resolved = resolve("/tmp/does-not-exist-preview-disk.qcow2");
        assert_eq!(
            resolved.cfg.firmware.ovmf_code_path,
            PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")
        );
        assert_eq!(
            resolved.ovmf_vars_template,
            Some(PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd"))
        );
        assert_eq!(
            resolved.cfg.firmware.ovmf_vars_path,
            resolved.instance_dir.join("VARS.fd")
        );
        assert!(resolved.validation.is_ok());
    }

    #[test]
    fn a_fresh_disk_is_relocated_into_the_instance_directory() {
        let resolved = resolve("/tmp/does-not-exist-preview-disk.qcow2");
        assert_eq!(
            resolved.cfg.disk.path,
            resolved
                .instance_dir
                .join("does-not-exist-preview-disk.qcow2")
        );
    }

    #[test]
    fn validation_verdict_comes_from_the_resolved_config() {
        let mut draft = draft("/tmp/does-not-exist-preview-disk.qcow2");
        draft.cpu = Some(andler_core::CpuConfig {
            cores: 0,
            ..andler_core::CpuConfig::reference_default()
        });
        let creation =
            Creation::resolve(&draft, "/tmp/instances".to_string(), &host()).expect("resolves");
        let resolved = Resolved::from_creation(&creation, &host());
        assert_eq!(
            resolved
                .validation
                .as_ref()
                .expect_err("zero cores must fail"),
            "cpu.cores must be at least 1"
        );
        assert!(
            report_dry_run(&resolved, false).is_err(),
            "--dry-run must refuse a config the daemon would reject"
        );
    }
}
