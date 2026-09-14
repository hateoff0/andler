use andler_core::InstanceKind;
use serde::Serialize;

use crate::helpers::{emit_json, format_bytes};
use crate::preview::Resolved;

#[derive(Serialize)]
struct VerifyReport {
    name: String,
    passed: bool,
    checks: Vec<VerifyCheck>,
}

#[derive(Serialize)]
struct VerifyCheck {
    name: &'static str,
    ok: bool,
    detail: String,
}
struct Check {
    name: &'static str,
    result: Result<String, String>,
}

fn run_checks(resolved: &Resolved, checks: Vec<Check>, json: bool) -> bool {
    if json {
        // Structured report for `create --verify --json`: one entry per check
        // with its pass/fail and human-readable detail, plus the aggregate
        // `passed` flag. Kept separate from the human printer below so the
        // wire shape stays stable while the text output can keep evolving.
        let mut report_checks = Vec::with_capacity(checks.len());
        let mut all_passed = true;
        for check in &checks {
            let (ok, detail) = match &check.result {
                Ok(detail) => (true, detail.clone()),
                Err(detail) => {
                    all_passed = false;
                    (false, detail.clone())
                }
            };
            report_checks.push(VerifyCheck {
                name: check.name,
                ok,
                detail,
            });
        }
        let report = VerifyReport {
            name: resolved.cfg.name.clone(),
            passed: all_passed,
            checks: report_checks,
        };
        if let Err(e) = emit_json(&report) {
            eprintln!("failed to emit verify report as JSON: {e}");
        }
        return all_passed;
    }

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
    let result = match disk.path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() && !parent.exists() => {
            Err(format!("directory does not exist: {}", parent.display()))
        }
        _ => Ok(format!(
            "{:?}, {} GiB, {}",
            disk.format,
            disk.size_bytes / andler_core::DiskConfig::GIB,
            disk.path.display()
        )),
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
    let result = match &resolved.cfg.kind {
        InstanceKind::LinuxVm { iso_path, .. } => {
            if iso_path.as_os_str().is_empty() {
                Ok("none given — will boot from disk".to_string())
            } else if iso_path.exists() {
                Ok(iso_path.display().to_string())
            } else {
                Err(format!("file not found: {}", iso_path.display()))
            }
        }
        InstanceKind::AndroidVm { .. } => Ok("not applicable to Android VMs".to_string()),
    };
    Check {
        name: "ISO image",
        result,
    }
}

fn check_base_image(resolved: &Resolved) -> Check {
    let result = match &resolved.cfg.kind {
        InstanceKind::AndroidVm { android_profile } => match &resolved.android_base_image {
            Some(path) if std::path::Path::new(path).exists() => Ok(path.clone()),
            Some(path) => Err(format!("file not found: {path}")),
            None => andler_core::base_image::resolve(android_profile)
                .map(|path| format!("auto-resolved: {}", path.display()))
                .map_err(|e| e.to_string()),
        },
        InstanceKind::LinuxVm { .. } => Ok("not applicable to Linux VMs".to_string()),
    };
    Check {
        name: "Base image",
        result,
    }
}

fn check_disk_mode(resolved: &Resolved) -> Check {
    let result = match &resolved.cfg.kind {
        InstanceKind::AndroidVm { .. } => Ok(if resolved.cfg.disk.base_image.is_some() {
            "linked overlay (backing file: base image)".to_string()
        } else {
            "full copy (independent of base image)".to_string()
        }),
        InstanceKind::LinuxVm { .. } => Ok("standalone disk (no base image)".to_string()),
    };
    Check {
        name: "Disk mode",
        result,
    }
}

/// The resolver-side sanity gate, reported from the verdict the one resolver
/// already computed: `--verify` and `--dry-run` and the daemon can never
/// disagree about whether a config is creatable.
fn check_config_sanity(resolved: &Resolved) -> Check {
    let result = match &resolved.validation {
        Ok(()) => Ok(format!(
            "{} cores, {} GiB RAM, {} MiB GPU",
            resolved.cfg.cpu.cores,
            resolved.cfg.memory.size_bytes / andler_core::MemoryConfig::GIB,
            resolved.cfg.gpu.hostmem_bytes / andler_core::GpuConfig::MIB
        )),
        Err(issue) => Err(issue.clone()),
    };
    Check {
        name: "Config sanity",
        result,
    }
}

/// Informational host-RAM check: the real multi-instance overcommit gate runs
/// on the daemon (it alone sees every running instance), but the operator
/// should still see host RAM vs the requested size before starting so the
/// `MemoryOvercommit` refusal makes sense at a glance.
fn check_host_ram(resolved: &Resolved) -> Check {
    let host_total = read_host_total_ram();
    let requested = resolved.cfg.memory.size_bytes;
    let result = match host_total {
        Some(total) => {
            if requested > total {
                Err(format!(
                    "host {} < requested {} (this instance alone exceeds host RAM)",
                    format_bytes(total),
                    format_bytes(requested)
                ))
            } else {
                Ok(format!(
                    "host {} — requested {} ({:.1}% of host)",
                    format_bytes(total),
                    format_bytes(requested),
                    (requested as f64 / total as f64) * 100.0
                ))
            }
        }
        None => Ok("not available — the daemon enforces the multi-instance gate".to_string()),
    };
    Check {
        name: "Host memory",
        result,
    }
}

/// Host physical RAM in bytes from `/proc/meminfo` (MemTotal), or `None` when
/// the file is unreadable. Mirrors the daemon's own read so the CLI and daemon
/// agree on the number they compare against.
fn read_host_total_ram() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in raw.lines() {
        let (key, value) = line.split_once(':')?;
        if key.trim() == "MemTotal" {
            // "<value> kB" — the value is in kilobytes. The field is
            // whitespace-padded, so trim before splitting off the unit.
            let (num, unit) = value.trim().split_once(' ')?;
            if unit.trim() == "kB" {
                if let Ok(kb) = num.parse::<u64>() {
                    return Some(kb.saturating_mul(1024));
                }
            }
        }
    }
    None
}

pub(crate) fn verify(resolved: &Resolved, json: bool) -> Result<bool, Box<dyn std::error::Error>> {
    let kind_requires_uefi = matches!(resolved.cfg.kind, InstanceKind::AndroidVm { .. });

    let mut checks = Vec::with_capacity(6);
    if kind_requires_uefi {
        checks.push(check_base_image(resolved));
        checks.push(check_disk_mode(resolved));
    } else {
        checks.push(check_iso(resolved));
    }
    checks.push(check_disk(resolved));
    checks.push(check_ovmf(resolved, kind_requires_uefi));
    checks.push(check_config_sanity(resolved));
    checks.push(check_host_ram(resolved));

    Ok(run_checks(resolved, checks, json))
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
            schema_version: andler_core::CURRENT_SCHEMA_VERSION,
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
            autostart: false,
        };
        let validation = cfg.validate();
        Resolved {
            cfg,
            instance_dir: disk_path.parent().map(PathBuf::from).unwrap_or_default(),
            ovmf_vars_template: None,
            android_base_image: None,
            validation,
        }
    }

    #[test]
    fn check_disk_passes_for_existing_directory_and_nonzero_size() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        let check = check_disk(&resolved);
        assert!(check.result.is_ok());
    }

    #[test]
    fn config_sanity_fails_on_zero_disk_size() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.disk.size_bytes = 0;
        resolved.validation = resolved.cfg.validate();
        assert!(check_config_sanity(&resolved).result.is_err());
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
    fn config_sanity_passes_for_sane_values() {
        let resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        assert!(check_config_sanity(&resolved).result.is_ok());
    }

    #[test]
    fn config_sanity_fails_on_out_of_range_gpu_for_hardware_backend() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.gpu.render_backend = andler_core::RenderBackend::Venus;
        resolved.cfg.gpu.hostmem_bytes = 64 * andler_core::GpuConfig::MIB;
        resolved.validation = resolved.cfg.validate();
        assert!(check_config_sanity(&resolved).result.is_err());
    }

    #[test]
    fn config_sanity_allows_small_hostmem_for_cpu_backend() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.gpu.render_backend = andler_core::RenderBackend::Cpu;
        resolved.cfg.gpu.hostmem_bytes = 64 * andler_core::GpuConfig::MIB;
        resolved.validation = resolved.cfg.validate();
        assert!(check_config_sanity(&resolved).result.is_ok());
    }

    #[test]
    fn config_sanity_fails_on_zero_cores() {
        let mut resolved = fixture_resolved(std::env::temp_dir().join("disk.qcow2"));
        resolved.cfg.cpu.cores = 0;
        resolved.validation = resolved.cfg.validate();
        assert!(check_config_sanity(&resolved).result.is_err());
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
        assert!(!run_checks(&resolved, checks, false));
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
        assert!(run_checks(&resolved, checks, false));
    }
    #[test]
    fn verify_report_serializes_spec_field_names() {
        let report = VerifyReport {
            name: "test-vm".to_string(),
            passed: false,
            checks: vec![
                VerifyCheck {
                    name: "Disk",
                    ok: true,
                    detail: "ok".to_string(),
                },
                VerifyCheck {
                    name: "OVMF firmware",
                    ok: false,
                    detail: "missing".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Every key the CLI consumer relies on must be present with the right type.
        assert_eq!(parsed["name"], "test-vm");
        assert_eq!(parsed["passed"], false);
        assert!(parsed["checks"].is_array());
        assert_eq!(parsed["checks"][0]["name"], "Disk");
        assert_eq!(parsed["checks"][0]["ok"], true);
        assert_eq!(parsed["checks"][0]["detail"], "ok");
        assert_eq!(parsed["checks"][1]["name"], "OVMF firmware");
        assert_eq!(parsed["checks"][1]["ok"], false);
    }
}
