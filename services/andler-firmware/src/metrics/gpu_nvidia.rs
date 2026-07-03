//! NVIDIA GPU metrics via the `nvidia-smi` CLI.

use andler_core::ResourceMetrics;

/// Checks if `nvidia-smi` binary is available in PATH.
pub(super) fn is_nvidia_available() -> bool {
    std::process::Command::new("nvidia-smi")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Reads NVIDIA GPU metrics via `nvidia-smi` CLI.
///
/// Output format of `--query-gpu=memory.used,memory.total,utilization.gpu
/// --format=csv,noheader,nounits`:
/// ```text
/// 1024, 8192, 67
/// ```
/// Values: MiB, MiB, percent.
pub(super) fn read_nvidia_metrics() -> Option<ResourceMetrics> {
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

    // Handle comma-separated values (may have spaces after commas)
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
        // Should return false in CI/test environments without nvidia-smi
        let _ = is_nvidia_available();
    }

    #[test]
    fn read_nvidia_metrics_without_nvidia_smi_returns_none() {
        // If nvidia-smi is not in PATH, should return None, not panic
        if !is_nvidia_available() {
            assert!(read_nvidia_metrics().is_none());
        }
    }

    #[test]
    fn parse_nvidia_smi_output() {
        // Simulate nvidia-smi output parsing
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
