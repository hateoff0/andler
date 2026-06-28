//! Collection of GPU metrics from host-side sysfs and vendor tools.
//!
//! QEMU QMP does not provide GPU metrics (no `query-gpus`), so data is
//! read directly from sysfs or vendor CLI tools — same level as `/proc`
//! for CPU/RAM. Supported vendors:
//!
//! | Vendor | Source | Metrics |
//! |--------|--------|---------|
//! | AMD | sysfs (`mem_info_vram_*`, `gpu_busy_percent`) | VRAM used/total, GPU load % |
//! | NVIDIA | `nvidia-smi` CLI | VRAM used/total, GPU load % |
//! | Intel | sysfs `i915` (`busyiffies`, `mem_info_*`) | GPU load % (delta-based), VRAM (stolen, approximate) |
//!
//! Vendor detection priority: AMD → NVIDIA → Intel (first found wins).
//! Multi-GPU per-process binding is left for a future iteration.
//!
//! ## AMD
//!
//! Scans `/sys/class/drm/card*/device/mem_info_vram_used` — if the file
//! exists, it's an AMDGPU driver with VRAM metrics support. Reads VRAM
//! used/total and `gpu_busy_percent` directly.
//!
//! ## NVIDIA
//!
//! Runs `nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu
//! --format=csv,noheader,nounits`. Requires `nvidia-smi` in PATH.
//! If the binary is not found, returns None (no retry).
//!
//! ## Intel
//!
//! Reads `busyiffies` from `/sys/class/drm/card*/device/gt/gt0/attrs/`
//! and computes GPU load % as delta between consecutive calls (1s interval).
//! VRAM uses `mem_info_dev_local_mem_alloc` / `mem_info_stolen_local_mem`
//! (stolen memory — approximate, not full VRAM).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use andler_core::ResourceMetrics;

/// Base DRM sysfs directory.
const DRM_SYSFS_BASE: &str = "/sys/class/drm";

// ---------------------------------------------------------------------------
// AMD constants
// ---------------------------------------------------------------------------

const AMD_VRAM_USED: &str = "device/mem_info_vram_used";
const AMD_VRAM_TOTAL: &str = "device/mem_info_vram_total";
const AMD_GPU_BUSY: &str = "device/gpu_busy_percent";

// ---------------------------------------------------------------------------
// Intel constants
// ---------------------------------------------------------------------------

const INTEL_BUSYIFFIES: &str = "device/gt/gt0/attrs/busyiffies";
const INTEL_VRAM_USED: &str = "device/mem_info_dev_local_mem_alloc";
const INTEL_VRAM_TOTAL: &str = "device/mem_info_stolen_local_mem";

// ---------------------------------------------------------------------------
// Global state for Intel GPU load delta
// ---------------------------------------------------------------------------

/// Previous Intel busyiffies value for computing GPU load %.
/// AtomicU64 allows concurrent reads from the metrics poller without
/// holding a mutex. Initialized to u64::MAX as sentinel (no previous sample).
static PREV_INTEL_BUSYIFFIES: AtomicU64 = AtomicU64::new(u64::MAX);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Enum representing detected GPU vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
}

/// Reads a `u64` from a sysfs file. Returns `None` on any error.
fn read_sysfs_u64(base: &Path, relative: &str) -> Option<u64> {
    let content = std::fs::read_to_string(base.join(relative)).ok()?;
    content.trim().parse::<u64>().ok()
}

/// Reads an `f32` from a sysfs file. Returns `None` on any error.
fn read_sysfs_f32(base: &Path, relative: &str) -> Option<f32> {
    let content = std::fs::read_to_string(base.join(relative)).ok()?;
    content.trim().parse::<f32>().ok()
}

/// Reads a `u64` from a sysfs file (single integer, no unit suffix).
fn read_sysfs_u64_plain(path: &Path) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u64>().ok()
}

/// Scans `/sys/class/drm/card*` entries, sorted by name.
fn sorted_drm_cards() -> Vec<PathBuf> {
    let drm_base = Path::new(DRM_SYSFS_BASE);

    let mut entries: Vec<_> = match std::fs::read_dir(drm_base) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return Vec::new(),
    };

    entries.sort_by_key(|e| e.file_name());

    entries
        .into_iter()
        .filter_map(|e| {
            let name = e.file_name();
            let name_str = name.to_str()?;
            if name_str.starts_with("card") && !name_str.contains('-') {
                Some(e.path())
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// AMD
// ---------------------------------------------------------------------------

/// Finds the first AMD GPU card in sysfs.
fn find_amd_gpu_card() -> Option<PathBuf> {
    for card_path in sorted_drm_cards() {
        if card_path.join(AMD_VRAM_USED).exists() {
            return Some(card_path);
        }
    }
    None
}

/// Reads AMD GPU metrics from sysfs.
fn read_amd_metrics(card_path: &Path) -> ResourceMetrics {
    ResourceMetrics {
        vram_used_bytes: read_sysfs_u64(card_path, AMD_VRAM_USED),
        vram_total_bytes: read_sysfs_u64(card_path, AMD_VRAM_TOTAL),
        gpu_load_percent: read_sysfs_f32(card_path, AMD_GPU_BUSY),
        ..ResourceMetrics::default()
    }
}

// ---------------------------------------------------------------------------
// NVIDIA
// ---------------------------------------------------------------------------

/// Checks if `nvidia-smi` binary is available in PATH.
fn is_nvidia_available() -> bool {
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
fn read_nvidia_metrics() -> Option<ResourceMetrics> {
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

// ---------------------------------------------------------------------------
// Intel
// ---------------------------------------------------------------------------

/// Finds the first Intel GPU card in sysfs (i915 driver with gt/ attrs).
fn find_intel_gpu_card() -> Option<PathBuf> {
    for card_path in sorted_drm_cards() {
        if card_path.join(INTEL_BUSYIFFIES).exists() {
            return Some(card_path);
        }
    }
    None
}

/// Reads Intel GPU metrics from i915 sysfs.
///
/// GPU load is computed as delta of `busyiffies` between consecutive calls.
/// VRAM uses stolen memory (approximate — not full VRAM).
fn read_intel_metrics(card_path: &Path) -> ResourceMetrics {
    // GPU load: delta-based from busyiffies
    let current_busyiffies = read_sysfs_u64_plain(&card_path.join(INTEL_BUSYIFFIES));
    let gpu_load_percent = compute_intel_gpu_load(current_busyiffies);

    // VRAM: stolen memory (approximate)
    let vram_used_bytes = read_sysfs_u64_plain(&card_path.join(INTEL_VRAM_USED));
    let vram_total_bytes = read_sysfs_u64_plain(&card_path.join(INTEL_VRAM_TOTAL));

    ResourceMetrics {
        vram_used_bytes,
        vram_total_bytes,
        gpu_load_percent,
        ..ResourceMetrics::default()
    }
}

/// Computes Intel GPU load % from busyiffies delta.
///
/// `busyiffies` is a monotonically increasing counter of GPU busy time
/// in USER_HZ (typically 100 ticks/sec on Linux). GPU load = delta_busyiffies
/// / delta_time / USER_HZ * 100%.
///
/// First call always returns `None` (no previous sample). Subsequent calls
/// return load based on 1-second interval.
fn compute_intel_gpu_load(current_busyiffies: Option<u64>) -> Option<f32> {
    let current = current_busyiffies?;

    let prev = PREV_INTEL_BUSYIFFIES.swap(current, Ordering::Relaxed);

    // First call: sentinel value, no delta available
    if prev == u64::MAX {
        return None;
    }

    let delta_ticks = current.saturating_sub(prev);
    if delta_ticks == 0 {
        return Some(0.0);
    }

    // USER_HZ on Linux is typically 100 (HZ=100).
    // busyiffies counts in USER_HZ ticks.
    // Load = delta_ticks / (interval_secs * USER_HZ) * 100
    const USER_HZ: u64 = 100;
    const INTERVAL_SECS: f64 = 1.0; // metrics poller runs every 1 second

    let load = (delta_ticks as f64 / (INTERVAL_SECS * USER_HZ as f64) * 100.0) as f32;
    Some(load.clamp(0.0, 100.0))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Detects GPU vendor (AMD → NVIDIA → Intel priority) and returns metrics.
///
/// Returns `ResourceMetrics` with GPU fields populated if a compatible GPU
/// is found. All GPU fields will be `None` if no supported GPU is detected.
pub fn read_gpu_metrics() -> ResourceMetrics {
    // AMD (highest priority)
    if let Some(card_path) = find_amd_gpu_card() {
        return read_amd_metrics(&card_path);
    }

    // NVIDIA
    if is_nvidia_available() {
        if let Some(metrics) = read_nvidia_metrics() {
            return metrics;
        }
    }

    // Intel
    if let Some(card_path) = find_intel_gpu_card() {
        return read_intel_metrics(&card_path);
    }

    ResourceMetrics::default()
}

/// Merges GPU metrics into host metrics.
///
/// GPU fields from `gpu` fill `None` fields in `base`.
/// Existing `Some` fields in `base` are not overwritten.
pub fn merge_gpu_metrics(base: &mut ResourceMetrics, gpu: &ResourceMetrics) {
    if base.vram_used_bytes.is_none() {
        base.vram_used_bytes = gpu.vram_used_bytes;
    }
    if base.vram_total_bytes.is_none() {
        base.vram_total_bytes = gpu.vram_total_bytes;
    }
    if base.gpu_load_percent.is_none() {
        base.gpu_load_percent = gpu.gpu_load_percent;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- AMD tests --

    #[test]
    fn find_amd_gpu_card_does_not_panic() {
        let _ = find_amd_gpu_card();
    }

    // -- NVIDIA tests --

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

    // -- Intel tests --

    #[test]
    fn find_intel_gpu_card_does_not_panic() {
        let _ = find_intel_gpu_card();
    }

    #[test]
    fn compute_intel_gpu_load_first_call_returns_none() {
        // Reset sentinel
        PREV_INTEL_BUSYIFFIES.store(u64::MAX, Ordering::Relaxed);
        let result = compute_intel_gpu_load(Some(1000));
        assert!(result.is_none());
    }

    #[test]
    fn compute_intel_gpu_load_second_call_returns_value() {
        PREV_INTEL_BUSYIFFIES.store(1000, Ordering::Relaxed);
        // 10 ticks in 1 second at USER_HZ=100 = 10% load
        let result = compute_intel_gpu_load(Some(1010));
        assert!((result.unwrap() - 10.0).abs() < 0.1);
    }

    #[test]
    fn compute_intel_gpu_load_zero_delta_returns_zero() {
        PREV_INTEL_BUSYIFFIES.store(5000, Ordering::Relaxed);
        let result = compute_intel_gpu_load(Some(5000));
        assert!((result.unwrap() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn compute_intel_gpu_load_clamps_at_100() {
        PREV_INTEL_BUSYIFFIES.store(0, Ordering::Relaxed);
        // 200 ticks in 1 second = 200% → clamped to 100%
        let result = compute_intel_gpu_load(Some(200));
        assert!((result.unwrap() - 100.0).abs() < 0.1);
    }

    #[test]
    fn compute_intel_gpu_load_none_input_returns_none() {
        let result = compute_intel_gpu_load(None);
        assert!(result.is_none());
    }

    // -- read_gpu_metrics tests --

    #[test]
    fn read_gpu_metrics_does_not_panic() {
        let metrics = read_gpu_metrics();
        // Should not panic regardless of hardware
        assert!(metrics.cpu_percent.is_none());
        assert!(metrics.memory_used_bytes.is_none());
    }

    // -- merge tests --

    #[test]
    fn merge_gpu_metrics_fills_none_fields() {
        let mut base = ResourceMetrics {
            cpu_percent: Some(50.0),
            memory_used_bytes: Some(1024),
            ..ResourceMetrics::default()
        };
        let gpu = ResourceMetrics {
            vram_used_bytes: Some(256 * 1024 * 1024),
            vram_total_bytes: Some(8 * 1024 * 1024 * 1024),
            gpu_load_percent: Some(33.0),
            ..ResourceMetrics::default()
        };

        merge_gpu_metrics(&mut base, &gpu);

        assert_eq!(base.cpu_percent, Some(50.0));
        assert_eq!(base.memory_used_bytes, Some(1024));
        assert_eq!(base.vram_used_bytes, Some(256 * 1024 * 1024));
        assert_eq!(base.vram_total_bytes, Some(8 * 1024 * 1024 * 1024));
        assert!((base.gpu_load_percent.unwrap() - 33.0).abs() < f32::EPSILON);
    }

    #[test]
    fn merge_gpu_metrics_does_not_overwrite_existing() {
        let mut base = ResourceMetrics {
            vram_used_bytes: Some(100),
            vram_total_bytes: Some(200),
            gpu_load_percent: Some(50.0),
            ..ResourceMetrics::default()
        };
        let gpu = ResourceMetrics {
            vram_used_bytes: Some(999),
            vram_total_bytes: Some(888),
            gpu_load_percent: Some(10.0),
            ..ResourceMetrics::default()
        };

        merge_gpu_metrics(&mut base, &gpu);

        assert_eq!(base.vram_used_bytes, Some(100));
        assert_eq!(base.vram_total_bytes, Some(200));
        assert!((base.gpu_load_percent.unwrap() - 50.0).abs() < f32::EPSILON);
    }

    #[test]
    fn merge_gpu_metrics_empty_gpu_does_not_touch_base() {
        let mut base = ResourceMetrics {
            cpu_percent: Some(75.0),
            ..ResourceMetrics::default()
        };
        let gpu = ResourceMetrics::default();

        merge_gpu_metrics(&mut base, &gpu);

        assert_eq!(base.cpu_percent, Some(75.0));
        assert!(base.vram_used_bytes.is_none());
        assert!(base.vram_total_bytes.is_none());
        assert!(base.gpu_load_percent.is_none());
    }

    #[test]
    fn sorted_drm_cards_does_not_panic() {
        let _ = sorted_drm_cards();
    }
}
