//! Collection of GPU metrics from host-side sysfs and vendor tools.
//!
//! Lives in `andler-firmware`, not `backends/andler-qemu`, because GPU
//! metrics are host-level (not tied to any VM's QEMU PID) and read the
//! same sysfs paths/vendor tools as `detect::gpu` — detection and
//! monitoring belong in one place. Per-VM metrics (CPU%, RAM, disk I/O,
//! network I/O — all of which *do* need a PID) stay in
//! `backends/andler-qemu::metrics`, which calls into this module only for
//! the GPU fields.
//!
//! QEMU QMP does not provide GPU metrics (no `query-gpus`), so data is
//! read directly from sysfs or vendor CLI tools — same level as `/proc`
//! for CPU/RAM. Supported vendors:
//!
//! | Vendor | Source | Metrics |
//! |--------|--------|---------|
//! | AMD | sysfs (`mem_info_vram_*`, `gpu_busy_percent`) | VRAM used/total, GPU load % |
//! | NVIDIA | `nvidia-smi` CLI | VRAM used/total, GPU load % |
//! | Intel | sysfs `i915` (`power/rc6_residency_ms`) | GPU load % (idle-time-based), no VRAM |
//!
//! Vendor detection priority: AMD → NVIDIA → Intel (first found wins).
//! Multi-GPU per-process binding is left for a future iteration. See
//! `gpu_amd`/`gpu_nvidia`/`gpu_intel` for the vendor-specific details.

mod gpu_amd;
mod gpu_intel;
mod gpu_nvidia;

use std::path::{Path, PathBuf};

use andler_core::ResourceMetrics;

/// Base DRM sysfs directory.
const DRM_SYSFS_BASE: &str = "/sys/class/drm";

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

/// Detects GPU vendor (AMD → NVIDIA → Intel priority) and returns metrics.
///
/// Returns `ResourceMetrics` with GPU fields populated if a compatible GPU
/// is found. All GPU fields will be `None` if no supported GPU is detected.
pub fn read_gpu_metrics() -> ResourceMetrics {
    // AMD (highest priority)
    if let Some(card_path) = gpu_amd::find_amd_gpu_card() {
        return gpu_amd::read_amd_metrics(&card_path);
    }

    // NVIDIA
    if gpu_nvidia::is_nvidia_available() {
        if let Some(metrics) = gpu_nvidia::read_nvidia_metrics() {
            return metrics;
        }
    }

    // Intel
    if let Some(card_path) = gpu_intel::find_intel_gpu_card() {
        return gpu_intel::read_intel_metrics(&card_path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_gpu_metrics_does_not_panic() {
        let metrics = read_gpu_metrics();
        // Should not panic regardless of hardware
        assert!(metrics.cpu_percent.is_none());
        assert!(metrics.memory_used_bytes.is_none());
    }

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
