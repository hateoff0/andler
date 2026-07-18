//! `--dry-run` — client-side preview of what `andler create` would send
//! to the daemon and (best-effort) the resulting QEMU command line,
//! without ever contacting the daemon. See ROADMAP.md, "CLI: add
//! --dry-run flag to preview QEMU args before creating".
//!
//! This deliberately does **not** call the daemon at all — it mirrors the
//! daemon's own resolution logic client-side:
//! - `InstanceConfig::try_from(request)` / `AndroidProfile::resolve(...)` —
//!   the exact same conversions the daemon uses (`andler_rpc::convert`,
//!   `andler_core::android_profile`).
//! - OVMF auto-detection via `andler_firmware::detect_matched_pair()` —
//!   the same call the daemon makes at startup (see `daemon::firmware`).
//! - The disk-path relocation logic from
//!   `Daemon::create_linux_instance`/`create_android_instance` (a fresh
//!   qcow2 disk/overlay always ends up at `<instance_dir>/disk.qcow2`,
//!   OVMF VARS at `<instance_dir>/VARS.fd`) — duplicated here rather than
//!   shared, since the daemon's version is inseparably interleaved with
//!   the actual `mkdir`/file-copy/qcow2-create side effects this preview
//!   must not perform.
//!
//! The instance id and instance directory shown are **placeholders** —
//! the daemon assigns the real id (and therefore the real directory) only
//! at actual creation time. Everything else (resolved config, QEMU argv)
//! is accurate as of the moment this runs; if the host's GPU/OVMF/audio
//! auto-detection would give a different answer by the time you actually
//! run `andler create` for real (e.g. you install `edk2-ovmf` in
//! between), the preview and the real creation can diverge — same as any
//! other auto-detected default would.

use std::path::PathBuf;

use andler_core::{AndroidProfile, DiskFormat, InstanceConfig, InstanceId, InstanceKind};
use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};

/// Resolved config plus the two placeholder/preview-only values that
/// don't come from the config itself: the instance directory (derived
/// from a placeholder id — see module doc), and the OVMF VARS *template*
/// path (as opposed to `cfg.firmware.ovmf_vars_path`, which after
/// resolution is always `<instance_dir>/VARS.fd` — the template is what
/// would be *copied* there, useful to show/check separately). `None`
/// means no OVMF was found at all (Legacy BIOS for Linux, a hard error
/// for Android — see `verify.rs`).
pub(crate) struct Resolved {
    pub cfg: InstanceConfig,
    pub instance_dir: PathBuf,
    pub ovmf_vars_template: Option<PathBuf>,
}

pub fn print_linux_preview(req: &CreateInstanceRequest) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_linux(req)?;
    print_preview(&resolved);
    Ok(())
}

pub fn print_android_preview(
    req: &CreateAndroidInstanceRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_android(req)?;
    print_preview(&resolved);
    Ok(())
}

/// Mirrors `daemon/src/service.rs::create_instance` + the disk-relocation
/// branch of `Daemon::create_linux_instance` — see module doc comment for
/// why this is duplicated rather than shared with the daemon (the
/// daemon's version is inseparable from real side effects this must not
/// perform). Used by both `--dry-run` (`print_linux_preview`) and
/// `--verify` (`verify::verify_linux`).
pub(crate) fn resolve_linux(req: &CreateInstanceRequest) -> Result<Resolved, Box<dyn std::error::Error>> {
    let mut cfg = InstanceConfig::try_from(req.clone())?;
    let id = InstanceId::new();
    let instance_dir = andler_core::paths::instances_root().join(id.0.to_string());

    // Client-supplied `--ovmf-vars-template` wins; otherwise fall back to
    // auto-detection.
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

/// See `resolve_linux` — the Android counterpart, mirroring
/// `Daemon::create_android_instance` (minus the actual overlay-file
/// creation, which `andler_disk::overlay::create_overlay` performs for
/// real and this must not).
pub(crate) fn resolve_android(
    req: &CreateAndroidInstanceRequest,
) -> Result<Resolved, Box<dyn std::error::Error>> {
    let profile_msg = req
        .profile
        .clone()
        .ok_or("CreateAndroidInstanceRequest is missing `profile`")?;
    let profile = AndroidProfile::try_from(profile_msg)?;

    let id = InstanceId::new();
    let instances_root = if req.instances_root.is_empty() {
        andler_core::paths::instances_root()
    } else {
        PathBuf::from(&req.instances_root)
    };
    let instance_dir = instances_root.join(id.0.to_string());

    let base_image_path = PathBuf::from(&req.base_image_path);
    // Same naming as `andler_disk::overlay::create_overlay` — see module
    // doc comment for why this is duplicated rather than called (that
    // function actually creates the qcow2 file on disk).
    let overlay_path = instance_dir.join("disk.qcow2");
    let ovmf_vars_path = instance_dir.join("VARS.fd");

    let ovmf_vars_template = if req.ovmf_vars_template.is_empty() {
        andler_firmware::detect_matched_pair()
            .ok()
            .map(|found| found.vars_template)
    } else {
        Some(PathBuf::from(&req.ovmf_vars_template))
    };

    let mut cfg = profile.resolve(
        req.name.clone(),
        base_image_path,
        overlay_path,
        req.overlay_size_bytes,
        ovmf_vars_path,
    );
    cfg.id = id;

    Ok(Resolved {
        cfg,
        instance_dir,
        ovmf_vars_template,
    })
}

/// Mirrors the "only relocate a disk that doesn't exist yet" branch of
/// `Daemon::create_linux_instance` exactly (same condition, same file-name
/// preservation) — everything else in that function is a real side
/// effect (`ensure_private_dir`, `qcow2::create`) this preview must not
/// perform.
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

fn print_preview(resolved: &Resolved) {
    let Resolved { cfg, instance_dir, ovmf_vars_template } = resolved;
    let ovmf_vars_template = ovmf_vars_template.as_deref();

    println!("─── Dry run: this would create ───────────────────────────────");
    println!("Name:            {}", cfg.name);

    match &cfg.kind {
        InstanceKind::LinuxVm { iso_path, cdrom_bus } => {
            println!("Type:            Linux VM");
            if iso_path.as_os_str().is_empty() {
                println!("ISO:             (none — boots from disk)");
            } else {
                println!("ISO:             {} (bus: {cdrom_bus:?})", iso_path.display());
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
        if cfg.display.fullscreen { ", fullscreen" } else { "" }
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
    let args = andler_qemu::cmdline::build_args(cfg, &qmp_placeholder);
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
}
