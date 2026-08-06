use std::sync::OnceLock;

use andler_core::ResourceMetrics;
use nvml_wrapper::Nvml;

fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| match Nvml::init() {
        Ok(nvml) => Some(nvml),
        Err(error) => {
            tracing::debug!(%error, "NVML unavailable, will try nvidia-smi CLI as fallback");
            None
        }
    })
    .as_ref()
}

pub(super) fn is_nvidia_available() -> bool {
    nvml().is_some() || nvidia_smi_available()
}

pub(super) fn read_nvidia_metrics() -> Option<ResourceMetrics> {
    if let Some(nvml) = nvml() {
        if let Some(metrics) = read_via_nvml(nvml) {
            return Some(metrics);
        }
    }
    read_via_nvidia_smi()
}

fn read_via_nvml(nvml: &Nvml) -> Option<ResourceMetrics> {
    let device = nvml.device_by_index(0).ok()?;
    let memory = device.memory_info().ok()?;
    let utilization = device.utilization_rates().ok()?;

    Some(ResourceMetrics {
        vram_used_bytes: Some(memory.used),
        vram_total_bytes: Some(memory.total),
        gpu_load_percent: Some((utilization.gpu as f32).clamp(0.0, 100.0)),
        ..ResourceMetrics::default()
    })
}

fn nvidia_smi_available() -> bool {
    std::process::Command::new("nvidia-smi")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn read_via_nvidia_smi() -> Option<ResourceMetrics> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?.trim();

    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 3 {
        return None;
    }

    let vram_used_mib: u64 = parts[0].parse().ok()?;
    let vram_total_mib: u64 = parts[1].parse().ok()?;
    let gpu_load: f32 = parts[2].parse().ok()?;

    Some(ResourceMetrics {
        vram_used_bytes: Some(vram_used_mib * 1024 * 1024),
        vram_total_bytes: Some(vram_total_mib * 1024 * 1024),
        gpu_load_percent: Some(gpu_load.clamp(0.0, 100.0)),
        ..ResourceMetrics::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_nvidia_available_does_not_panic() {
        let _ = is_nvidia_available();
    }

    #[test]
    fn read_nvidia_metrics_without_nvidia_returns_none() {
        if !is_nvidia_available() {
            assert!(read_nvidia_metrics().is_none());
        }
    }

    #[test]
    fn nvml_lookup_does_not_panic_and_is_cached() {
        let first = nvml().is_some();
        let second = nvml().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn parse_nvidia_smi_output() {
        let stdout = "1024, 8192, 67\n";
        let line = stdout.lines().next().unwrap().trim();
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "1024");
        assert_eq!(parts[1], "8192");
        assert_eq!(parts[2], "67");

        let vram_used_mib: u64 = parts[0].parse().unwrap();
        let vram_total_mib: u64 = parts[1].parse().unwrap();
        let gpu_load: f32 = parts[2].parse().unwrap();

        assert_eq!(vram_used_mib * 1024 * 1024, 1024 * 1024 * 1024);
        assert_eq!(vram_total_mib * 1024 * 1024, 8192 * 1024 * 1024);
        assert!((gpu_load - 67.0).abs() < f32::EPSILON);
    }
}
