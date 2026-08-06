use std::path::PathBuf;

use andler_core::{AndroidProfile, DiskFormat, InstanceConfig, InstanceId, InstanceKind};
use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};

pub(crate) struct Resolved {
    pub cfg: InstanceConfig,
    pub instance_dir: PathBuf,
    pub ovmf_vars_template: Option<PathBuf>,
}

pub fn print_linux_preview(req: &CreateInstanceRequest) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_linux(req)?;
    print_preview(&resolved)?;
    Ok(())
}

pub fn print_android_preview(
    req: &CreateAndroidInstanceRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_android(req)?;
    print_preview(&resolved)?;
    Ok(())
}

pub(crate) fn resolve_linux(
    req: &CreateInstanceRequest,
) -> Result<Resolved, Box<dyn std::error::Error>> {
    let mut cfg = InstanceConfig::try_from(req.clone())?;
    let id = InstanceId::new();
    let instance_dir = andler_core::paths::instances_root().join(id.0.to_string());

    let detected = if cfg.firmware.ovmf_code_path.as_os_str().is_empty()
        || cfg.firmware.ovmf_vars_path.as_os_str().is_empty()
    {
        andler_firmware::detect_matched_pair().ok()
    } else {
        None
    };

    if cfg.firmware.ovmf_code_path.as_os_str().is_empty() {
        if let Some(found) = &detected {
            cfg.firmware.ovmf_code_path = found.code.clone();
        }
    }

    let ovmf_vars_template = if cfg.firmware.ovmf_vars_path.as_os_str().is_empty() {
        detected.map(|found| found.vars_template)
    } else {
        Some(cfg.firmware.ovmf_vars_path.clone())
    };
    if ovmf_vars_template.is_some() {
        cfg.firmware.ovmf_vars_path = instance_dir.join("VARS.fd");
    }

    relocate_fresh_disk(&mut cfg, &instance_dir);
    cfg.id = id;

    Ok(Resolved {
        cfg,
        instance_dir,
        ovmf_vars_template,
    })
}

pub(crate) fn resolve_android(
    req: &CreateAndroidInstanceRequest,
) -> Result<Resolved, Box<dyn std::error::Error>> {
    let profile_msg = req
        .profile
        .ok_or("CreateAndroidInstanceRequest is missing `profile`")?;
    let profile = AndroidProfile::try_from(profile_msg)?;

    let id = InstanceId::new();
    let instances_root = if req.instances_root.is_empty() {
        andler_core::paths::instances_root()
    } else {
        PathBuf::from(&req.instances_root)
    };
    let instance_dir = instances_root.join(id.0.to_string());

    let base_image_path = if req.base_image_path.is_empty() {
        andler_core::base_image::resolve(&profile).unwrap_or_default()
    } else {
        PathBuf::from(&req.base_image_path)
    };
    let disk_path = instance_dir.join("disk.qcow2");
    let ovmf_vars_path = instance_dir.join("VARS.fd");

    let ovmf_vars_template = if req.ovmf_vars_template.is_empty() {
        andler_firmware::detect_matched_pair()
            .ok()
            .map(|found| found.vars_template)
    } else {
        Some(PathBuf::from(&req.ovmf_vars_template))
    };

    let disk = if req.linked_overlay {
        andler_core::DiskConfig::overlay(disk_path, base_image_path, req.overlay_size_bytes)
    } else {
        andler_core::DiskConfig::standalone(disk_path, req.overlay_size_bytes)
    };

    let mut cfg = profile.resolve(req.name.clone(), disk, ovmf_vars_path);
    cfg.id = id;

    Ok(Resolved {
        cfg,
        instance_dir,
        ovmf_vars_template,
    })
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
            println!("ARM translator:  {:?}", android_profile.arm_translator);
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
