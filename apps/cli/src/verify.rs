use andler_core::InstanceKind;
use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};

use crate::preview::{resolve_android, resolve_linux, Resolved};

struct Check {
    name: &'static str,
    result: Result<String, String>,
}

fn run_checks(resolved: &Resolved, checks: Vec<Check>) -> bool {
    println!("─── Verifying instance config ─────────────────────────────────");
    println!("Name: {}", resolved.cfg.name);
    println!();

    let mut all_passed = true;
    for check in &checks {
        match &check.result {
            Ok(detail) => println!("  ✓ {:<28} {detail}", check.name),
            Err(detail) => {
                println!("  ✗ {:<28} {detail}", check.name);
                all_passed = false;
            }
        }
    }
    println!();

    if all_passed {
        println!("All checks passed. Nothing was created — run without --verify to create.");
    } else {
        println!("One or more checks failed. Nothing was created.");
    }
    all_passed
}

fn check_disk(resolved: &Resolved) -> Check {
    let disk = &resolved.cfg.disk;
    let result = if disk.size_bytes == 0 {
        Err("disk size is 0 bytes".to_string())
    } else {
        match disk.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() && !parent.exists() => {
                Err(format!("directory does not exist: {}", parent.display()))
            }
            _ => Ok(format!(
                "{:?}, {} GiB, {}",
                disk.format,
                disk.size_bytes / andler_core::DiskConfig::GIB,
                disk.path.display()
            )),
        }
    };
    Check {
        name: "Disk",
        result,
    }
}

fn check_ovmf(resolved: &Resolved, kind_requires_uefi: bool) -> Check {
    let result = match &resolved.ovmf_vars_template {
        Some(template) if template.exists() => Ok(format!("found: {}", template.display())),
        Some(template) => Err(format!("template does not exist: {}", template.display())),
        None if kind_requires_uefi => {
            Err("not found — Android requires UEFI, Legacy BIOS is not supported".to_string())
        }
        None => Ok("not found — will fall back to Legacy BIOS".to_string()),
    };
    Check {
        name: "OVMF firmware",
        result,
    }
}

fn check_iso(resolved: &Resolved) -> Check {
    let InstanceKind::LinuxVm { iso_path, .. } = &resolved.cfg.kind else {
        unreachable!("check_iso is only called for Linux VMs");
    };
    let result = if iso_path.as_os_str().is_empty() {
        Ok("none given — will boot from disk".to_string())
    } else if iso_path.exists() {
        Ok(iso_path.display().to_string())
    } else {
        Err(format!("file not found: {}", iso_path.display()))
    };
    Check {
        name: "ISO image",
        result,
    }
}

fn check_gpu_memory(resolved: &Resolved) -> Check {
    let mib = resolved.cfg.gpu.hostmem_bytes / andler_core::GpuConfig::MIB;
    let result = if !(256..=16384).contains(&mib) {
        Err(format!(
            "{mib} MiB is outside the sane range (256-16384 MiB)"
        ))
    } else {
        Ok(format!("{mib} MiB"))
    };
    Check {
        name: "GPU memory",
        result,
    }
}

fn check_cpu_memory(resolved: &Resolved) -> Check {
    let cores = resolved.cfg.cpu.cores;
    let gib = resolved.cfg.memory.size_bytes / andler_core::MemoryConfig::GIB;
    let result = if cores == 0 {
        Err("0 CPU cores requested".to_string())
    } else if gib == 0 {
        Err("0 GiB memory requested".to_string())
    } else {
        Ok(format!("{cores} cores, {gib} GiB"))
    };
    Check {
        name: "CPU/Memory",
        result,
    }
}

pub fn verify_linux(req: &CreateInstanceRequest) -> Result<bool, Box<dyn std::error::Error>> {
    let resolved = resolve_linux(req)?;

    let checks = vec![
        check_iso(&resolved),
        check_disk(&resolved),
        check_ovmf(&resolved, false),
        check_gpu_memory(&resolved),
        check_cpu_memory(&resolved),
    ];

    Ok(run_checks(&resolved, checks))
}

pub fn verify_android(
    req: &CreateAndroidInstanceRequest,
) -> Result<bool, Box<dyn std::error::Error>> {
    let resolved = resolve_android(req)?;

    let base_image_check = Check {
        name: "Base image",
        result: if req.base_image_path.is_empty() {
            match req.profile {
                None => Err("missing profile".to_string()),
                Some(profile_msg) => match andler_core::AndroidProfile::try_from(profile_msg) {
                    Err(e) => Err(e.to_string()),
                    Ok(profile) => andler_core::base_image::resolve(&profile)
                        .map(|path| format!("auto-resolved: {}", path.display()))
                        .map_err(|e| e.to_string()),
                },
            }
        } else if std::path::Path::new(&req.base_image_path).exists() {
            Ok(req.base_image_path.clone())
        } else {
            Err(format!("file not found: {}", req.base_image_path))
        },
    };

    let disk_mode_check = Check {
        name: "Disk mode",
        result: Ok(if req.linked_overlay {
            "linked overlay (backing file: base image)".to_string()
        } else {
            "full copy (independent of base image)".to_string()
        }),
    };

    let checks = vec![
        base_image_check,
        disk_mode_check,
        check_disk(&resolved),
        check_ovmf(&resolved, true),
        check_gpu_memory(&resolved),
        check_cpu_memory(&resolved),
    ];
    Ok(run_checks(&resolved, checks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceConfig, InstanceId, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn fixture_resolved(disk_path: PathBuf) -> Resolved {
        let cfg = InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::new(),
                cdrom_bus: andler_core::CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(disk_path.clone()),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::new()),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        };
        Resolved {
            cfg,
            instance_dir: disk_path.parent().map(PathBuf::from).unwrap_or_default(),
            ovmf_vars_template: None,
        }
    }

    #[test]
    fn check_disk_passes_for_existing_directory_and_nonzero_size() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let check = check_disk(&resolved);
        assert!(check.result.is_ok());
    }

    #[test]
    fn check_disk_fails_for_zero_size() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.disk.size_bytes = 0;
        let check = check_disk(&resolved);
        assert!(check.result.is_err());
    }

    #[test]
    fn check_disk_fails_for_nonexistent_directory() {
        let resolved = fixture_resolved(PathBuf::from(
            "/andler-test-path-that-should-not-exist-anywhere/disk.qcow2",
        ));
        let check = check_disk(&resolved);
        assert!(check.result.is_err());
    }

    #[test]
    fn check_ovmf_passes_when_none_and_not_required() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let check = check_ovmf(&resolved, false);
        assert!(
            check.result.is_ok(),
            "Legacy BIOS fallback is a pass for Linux"
        );
    }

    #[test]
    fn check_ovmf_fails_when_none_and_required() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let check = check_ovmf(&resolved, true);
        assert!(check.result.is_err(), "Android requires UEFI");
    }

    #[test]
    fn check_ovmf_fails_when_template_path_does_not_exist() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.ovmf_vars_template = Some(PathBuf::from(
            "/andler-test-path-that-should-not-exist-anywhere/VARS.fd",
        ));
        let check = check_ovmf(&resolved, false);
        assert!(check.result.is_err());
    }

    #[test]
    fn check_ovmf_passes_when_template_exists() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.ovmf_vars_template = Some(std::env::temp_dir());
        let check = check_ovmf(&resolved, true);
        assert!(check.result.is_ok());
    }

    #[test]
    fn check_iso_passes_when_empty() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let check = check_iso(&resolved);
        assert!(check.result.is_ok());
    }

    #[test]
    fn check_iso_fails_when_nonexistent() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.kind = InstanceKind::LinuxVm {
            iso_path: PathBuf::from("/andler-test-path-that-should-not-exist-anywhere.iso"),
            cdrom_bus: andler_core::CdromBus::Ide,
        };
        let check = check_iso(&resolved);
        assert!(check.result.is_err());
    }

    #[test]
    fn check_gpu_memory_passes_within_range() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.gpu.hostmem_bytes = 4096 * andler_core::GpuConfig::MIB;
        assert!(check_gpu_memory(&resolved).result.is_ok());
    }

    #[test]
    fn check_gpu_memory_fails_below_range() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.gpu.hostmem_bytes = 64 * andler_core::GpuConfig::MIB;
        assert!(check_gpu_memory(&resolved).result.is_err());
    }

    #[test]
    fn check_gpu_memory_fails_above_range() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.gpu.hostmem_bytes = 32768 * andler_core::GpuConfig::MIB;
        assert!(check_gpu_memory(&resolved).result.is_err());
    }

    #[test]
    fn check_cpu_memory_fails_on_zero_cores() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.cpu.cores = 0;
        assert!(check_cpu_memory(&resolved).result.is_err());
    }

    #[test]
    fn check_cpu_memory_fails_on_zero_memory() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.memory.size_bytes = 0;
        assert!(check_cpu_memory(&resolved).result.is_err());
    }

    #[test]
    fn check_cpu_memory_passes_for_sane_values() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        assert!(check_cpu_memory(&resolved).result.is_ok());
    }

    #[test]
    fn run_checks_returns_false_if_any_check_failed() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let checks = vec![
            Check {
                name: "a",
                result: Ok("fine".to_string()),
            },
            Check {
                name: "b",
                result: Err("broken".to_string()),
            },
        ];
        assert!(!run_checks(&resolved, checks));
    }

    #[test]
    fn run_checks_returns_true_if_all_passed() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let checks = vec![
            Check {
                name: "a",
                result: Ok("fine".to_string()),
            },
            Check {
                name: "b",
                result: Ok("also fine".to_string()),
            },
        ];
        assert!(run_checks(&resolved, checks));
    }
}
