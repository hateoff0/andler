

use std::process::Command;

use andler_core::{DisplayEngine, RenderBackend};


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuVendor {
    Amd,
    Intel,
    Nvidia,
    Unknown,
    None,
}


pub(crate) fn detect_gpu_defaults() -> (RenderBackend, DisplayEngine, bool) {
    let vendor = detect_gpu_vendor();

    let (base_render, display_engine) = match vendor {
        GpuVendor::Nvidia => (RenderBackend::Venus, DisplayEngine::Sdl),
        GpuVendor::Amd | GpuVendor::Intel => (RenderBackend::Venus, DisplayEngine::Gtk),
        GpuVendor::Unknown => (RenderBackend::Venus, DisplayEngine::Sdl),
        GpuVendor::None => (RenderBackend::Cpu, DisplayEngine::None),
    };

    if vendor == GpuVendor::None {
        return (RenderBackend::Cpu, DisplayEngine::None, false);
    }

    let venus_supported = check_venus_requirements();
    let render_backend = if venus_supported {
        base_render
    } else {
        RenderBackend::VirGl
    };

    (render_backend, display_engine, venus_supported)
}


fn detect_gpu_vendor() -> GpuVendor {
    if let Some(vendor) = detect_gpu_vendor_via_sysfs() {
        return vendor;
    }
    detect_gpu_vendor_via_lspci().unwrap_or(GpuVendor::None)
}


const PCI_VENDOR_AMD: &str = "0x1002";
const PCI_VENDOR_NVIDIA: &str = "0x10de";
const PCI_VENDOR_INTEL: &str = "0x8086";

fn detect_gpu_vendor_via_sysfs() -> Option<GpuVendor> {
    let drm_dir = std::fs::read_dir("/sys/class/drm").ok()?;

    let mut found_intel = false;

    for entry in drm_dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let vendor_path = entry.path().join("device/vendor");
        let Ok(raw) = std::fs::read_to_string(&vendor_path) else {
            continue;
        };
        match raw.trim() {
            PCI_VENDOR_AMD => return Some(GpuVendor::Amd),
            PCI_VENDOR_NVIDIA => return Some(GpuVendor::Nvidia),
            PCI_VENDOR_INTEL => found_intel = true,
            _ => {}
        }
    }

    if found_intel {
        return Some(GpuVendor::Intel);
    }
    None
}

fn detect_gpu_vendor_via_lspci() -> Option<GpuVendor> {
    let output = Command::new("lspci").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    let vga_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("vga compatible controller") || l.contains("3d controller"))
        .collect();

    if vga_lines.iter().any(|l| l.contains("amd") || l.contains("ati")) {
        return Some(GpuVendor::Amd);
    }
    if vga_lines.iter().any(|l| l.contains("nvidia")) {
        return Some(GpuVendor::Nvidia);
    }
    if vga_lines.iter().any(|l| l.contains("intel")) {
        return Some(GpuVendor::Intel);
    }
    if vga_lines.is_empty() {
        None
    } else {
        Some(GpuVendor::Unknown)
    }
}


fn check_venus_requirements() -> bool {
    let Some(kernel) = kernel_version() else {
        return false;
    };
    let Some(qemu) = qemu_version() else {
        return false;
    };
    let vendor = detect_gpu_vendor();
    let mesa = mesa_version();

    venus_requirements_met(vendor, kernel, qemu, mesa)
}


fn venus_requirements_met(
    vendor: GpuVendor,
    kernel: (u32, u32, u32),
    qemu: (u32, u32, u32),
    mesa: Option<(u32, u32, u32)>,
) -> bool {
    if vendor == GpuVendor::Nvidia {
        return kernel >= (6, 13, 0) && qemu >= (9, 2, 0);
    }

    match mesa {
        Some(mesa) => kernel >= (6, 13, 0) && qemu >= (9, 2, 0) && mesa >= (24, 2, 0),
        None => false,
    }
}

fn kernel_version() -> Option<(u32, u32, u32)> {
    let output = Command::new("uname").arg("-r").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

fn qemu_version() -> Option<(u32, u32, u32)> {
    let output = Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

fn mesa_version() -> Option<(u32, u32, u32)> {
    let output = Command::new("glxinfo").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| l.contains("OpenGL version"))?;
    let mesa_marker = line.find("Mesa ")?;
    parse_version(&line[mesa_marker + "Mesa ".len()..])
}


fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let token = text
        .split(|c: char| c.is_whitespace())
        .find(|tok| {
            let mut parts = tok.split('.');
            parts.next().is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
                && parts.next().is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
        })?;

    let mut parts = token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts
        .next()
        .and_then(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok())
        .unwrap_or(0);

    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_three_components() {
        assert_eq!(parse_version("6.13.0"), Some((6, 13, 0)));
    }

    #[test]
    fn parse_version_handles_two_components() {
        assert_eq!(parse_version("QEMU emulator version 9.2.0"), Some((9, 2, 0)));
    }

    #[test]
    fn parse_version_handles_trailing_garbage() {
        assert_eq!(
            parse_version("4.6 (Compatibility Profile) Mesa 24.2.3"),
            Some((4, 6, 0))
        );
    }

    #[test]
    fn parse_version_extracts_mesa_suffix() {
        let line = "OpenGL version string: 4.6 (Compatibility Profile) Mesa 24.2.3";
        let mesa_marker = line.find("Mesa ").unwrap();
        assert_eq!(
            parse_version(&line[mesa_marker + "Mesa ".len()..]),
            Some((24, 2, 3))
        );
    }

    #[test]
    fn parse_version_returns_none_for_no_digits() {
        assert_eq!(parse_version("not a version at all"), None);
    }

    #[test]
    fn venus_requirements_met_nvidia_ignores_missing_mesa() {
        assert!(venus_requirements_met(
            GpuVendor::Nvidia,
            (6, 13, 0),
            (9, 2, 0),
            None,
        ));
    }

    #[test]
    fn venus_requirements_met_nvidia_still_checks_kernel_and_qemu() {
        assert!(!venus_requirements_met(
            GpuVendor::Nvidia,
            (6, 12, 0), // below minimum
            (9, 2, 0),
            None,
        ));
    }

    #[test]
    fn venus_requirements_met_amd_requires_mesa() {
        assert!(!venus_requirements_met(
            GpuVendor::Amd,
            (6, 13, 0),
            (9, 2, 0),
            None,
        ));
        assert!(venus_requirements_met(
            GpuVendor::Amd,
            (6, 13, 0),
            (9, 2, 0),
            Some((24, 2, 0)),
        ));
    }

    #[test]
    fn venus_requirements_met_amd_below_mesa_minimum_fails() {
        assert!(!venus_requirements_met(
            GpuVendor::Amd,
            (6, 13, 0),
            (9, 2, 0),
            Some((24, 1, 9)),
        ));
    }

    #[test]
    fn vendor_none_short_circuits_to_cpu_rendering() {
        let (render, display, venus) = match GpuVendor::None {
            GpuVendor::None => (RenderBackend::Cpu, DisplayEngine::None, false),
            _ => unreachable!(),
        };
        assert_eq!(render, RenderBackend::Cpu);
        assert_eq!(display, DisplayEngine::None);
        assert!(!venus);
    }
}
