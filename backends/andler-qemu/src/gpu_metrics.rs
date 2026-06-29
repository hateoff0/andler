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
//! | Intel | sysfs `i915` (`power/rc6_residency_ms`) | GPU load % (idle-time-based), no VRAM |
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
//! An earlier version of this module read a sysfs file named `busyiffies`
//! under `device/gt/gt0/attrs/` and VRAM from `mem_info_dev_local_mem_alloc`/
//! `mem_info_stolen_local_mem`. **None of those three paths exist** in the
//! real i915 sysfs tree (verified against `i915_sysfs.c`/the upstream `gt`
//! sysfs reorganization, see crate README) — the previous implementation
//! would always silently return `None` for Intel GPU load and VRAM on real
//! hardware, with no error and no test catching it (the unit tests only
//! exercised a hardcoded delta calculation, never the sysfs path itself).
//!
//! This version reads `device/power/rc6_residency_ms` — a long-standing,
//! documented i915 ABI: cumulative milliseconds the GPU has spent in the
//! RC6 idle power state since boot. GPU load is approximated as
//! `100% - (Δrc6_residency_ms / Δwall_clock_ms * 100)` between two calls,
//! using a real wall-clock delta (`Instant`), not an assumed fixed polling
//! interval. There is no equivalently simple, documented sysfs file for
//! integrated-Intel VRAM (stolen memory accounting lives in `debugfs`, not
//! `sysfs`, and isn't a stable userspace ABI) — rather than guess another
//! path, VRAM is left as `None` for Intel until a verified source exists.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

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

/// Documented i915 ABI (kept as a top-level compat path even after the
/// `gt/gt0/` sysfs reorganization — see crate README for the upstream
/// commit reference). Cumulative milliseconds the GPU has spent in the
/// RC6 idle power state since boot.
const INTEL_RC6_RESIDENCY_MS: &str = "device/power/rc6_residency_ms";

// ---------------------------------------------------------------------------
// Global state for Intel GPU load delta
// ---------------------------------------------------------------------------

/// Previous `(rc6_residency_ms, Instant)` sample for computing GPU load %
/// from a real elapsed-time delta. `Mutex`, not `AtomicU64` — need to store
/// two related values (residency + timestamp) atomically together, not
/// just a single counter; a plain `AtomicU64` for the counter alone would
/// let the residency and the timestamp it was paired with drift apart
/// under concurrent calls.
static PREV_INTEL_SAMPLE: Mutex<Option<(u64, Instant)>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Finds the first Intel GPU card in sysfs (i915 driver, identified by
/// presence of the `power/rc6_residency_ms` ABI file).
fn find_intel_gpu_card() -> Option<PathBuf> {
    for card_path in sorted_drm_cards() {
        if card_path.join(INTEL_RC6_RESIDENCY_MS).exists() {
            return Some(card_path);
        }
    }
    None
}

/// Reads Intel GPU metrics from i915 sysfs.
///
/// GPU load is approximated from the delta of `rc6_residency_ms` (time
/// spent idle) against real elapsed wall-clock time between two calls —
/// `100% - idle_fraction`. There is no VRAM metric for Intel (see module
/// doc comment for why): `vram_used_bytes`/`vram_total_bytes` are always
/// `None`.
fn read_intel_metrics(card_path: &Path) -> ResourceMetrics {
    let current_rc6_ms = read_sysfs_u64_plain(&card_path.join(INTEL_RC6_RESIDENCY_MS));
    let gpu_load_percent = compute_intel_gpu_load(current_rc6_ms);

    ResourceMetrics {
        gpu_load_percent,
        ..ResourceMetrics::default()
    }
}

/// Computes Intel GPU load % from the `rc6_residency_ms` delta and a real
/// elapsed wall-clock delta (not an assumed fixed polling interval — the
/// previous version of this function assumed exactly 1 second between
/// calls, which silently produced wrong numbers for any other polling
/// cadence). Bookkeeping (reading the clock, storing the previous sample)
/// lives here; the actual arithmetic is in `intel_gpu_load_from_delta`,
/// kept separate specifically so it can be unit-tested deterministically
/// without sleeping or mocking `Instant`.
///
/// First call always returns `None` (no previous sample to diff against).
fn compute_intel_gpu_load(current_rc6_ms: Option<u64>) -> Option<f32> {
    let current = current_rc6_ms?;
    let now = Instant::now();

    let mut guard = PREV_INTEL_SAMPLE.lock().unwrap_or_else(|e| e.into_inner());
    let prev = guard.replace((current, now));

    let (prev_rc6_ms, prev_instant) = prev?;
    let elapsed_ms = now.duration_since(prev_instant).as_millis() as f64;

    intel_gpu_load_from_delta(current.saturating_sub(prev_rc6_ms), elapsed_ms)
}

/// Pure arithmetic core of `compute_intel_gpu_load`: given how many
/// milliseconds of *idle* (RC6) time accumulated (`idle_delta_ms`) over a
/// real elapsed period (`elapsed_ms`), returns the GPU *busy* load
/// percentage: `100 - (idle_delta_ms / elapsed_ms * 100)`, clamped to
/// `[0, 100]` (rc6 accounting and the calling thread's clock aren't
/// perfectly synchronized, so a noisy sample could otherwise yield a
/// negative or >100% load).
///
/// Returns `None` if `elapsed_ms` isn't strictly positive — division by a
/// non-positive elapsed time is meaningless, not just numerically awkward.
fn intel_gpu_load_from_delta(idle_delta_ms: u64, elapsed_ms: f64) -> Option<f32> {
    if elapsed_ms <= 0.0 {
        return None;
    }

    let idle_fraction = (idle_delta_ms as f64 / elapsed_ms).clamp(0.0, 1.0);
    let load = ((1.0 - idle_fraction) * 100.0) as f32;
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
    fn intel_rc6_residency_path_is_the_real_documented_abi_not_busyiffies() {
        // Regression test for a real bug: an earlier version of this
        // module read a sysfs path/filename ("device/gt/gt0/attrs/
        // busyiffies") that does not exist anywhere in the real i915
        // sysfs tree — verified against the upstream `gt` sysfs
        // reorganization patch and `i915_sysfs.c` (see module doc comment
        // and crate README). `power/rc6_residency_ms` is the long-standing
        // documented ABI actually used.
        assert_eq!(INTEL_RC6_RESIDENCY_MS, "device/power/rc6_residency_ms");
        assert!(!INTEL_RC6_RESIDENCY_MS.contains("busyiffies"));
        assert!(!INTEL_RC6_RESIDENCY_MS.contains("attrs"));
    }

    // -- intel_gpu_load_from_delta: pure arithmetic, no time mocking needed --

    #[test]
    fn intel_gpu_load_from_delta_fully_idle_is_zero_load() {
        // 1000ms elapsed, all 1000ms of it spent idle (rc6) → 0% busy.
        let result = intel_gpu_load_from_delta(1000, 1000.0);
        assert!((result.unwrap() - 0.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_fully_busy_is_full_load() {
        // 1000ms elapsed, 0ms of it spent idle → 100% busy.
        let result = intel_gpu_load_from_delta(0, 1000.0);
        assert!((result.unwrap() - 100.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_partial_idle() {
        // 1000ms elapsed, 300ms idle → 70% busy.
        let result = intel_gpu_load_from_delta(300, 1000.0);
        assert!((result.unwrap() - 70.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_clamps_when_idle_exceeds_elapsed() {
        // Noisy/skewed sample: more "idle" ms reported than wall-clock ms
        // actually elapsed. Must clamp to 0% busy, not go negative.
        let result = intel_gpu_load_from_delta(1500, 1000.0);
        assert!((result.unwrap() - 0.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_non_positive_elapsed_returns_none() {
        assert!(intel_gpu_load_from_delta(100, 0.0).is_none());
        assert!(intel_gpu_load_from_delta(100, -5.0).is_none());
    }

    // -- compute_intel_gpu_load: real Instant/Mutex bookkeeping --
    //
    // These use a real (short) sleep with a generous tolerance rather than
    // mocking `Instant` (not mockable on stable Rust without an injected
    // clock abstraction, which would be a larger refactor than this fix
    // warrants). They share `PREV_INTEL_SAMPLE`, a single process-wide
    // static, with every other test in this module that calls
    // `compute_intel_gpu_load` — `#[test]` functions run on separate
    // threads concurrently by default, so without serialization these
    // tests could observe each other's writes. `serial_intel_sample_lock`
    // forces them onto a single lane.
    static INTEL_SAMPLE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn compute_intel_gpu_load_first_call_returns_none() {
        let _serial = INTEL_SAMPLE_TEST_LOCK.lock().unwrap();
        *PREV_INTEL_SAMPLE.lock().unwrap() = None;
        let result = compute_intel_gpu_load(Some(1000));
        assert!(result.is_none());
    }

    #[test]
    fn compute_intel_gpu_load_second_call_returns_a_clamped_percentage() {
        let _serial = INTEL_SAMPLE_TEST_LOCK.lock().unwrap();
        *PREV_INTEL_SAMPLE.lock().unwrap() = None;

        let first = compute_intel_gpu_load(Some(1000));
        assert!(first.is_none());

        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = compute_intel_gpu_load(Some(1010));
        let load = second.expect("second call must produce a value");
        assert!((0.0..=100.0).contains(&load));
    }

    #[test]
    fn compute_intel_gpu_load_none_input_returns_none() {
        let _serial = INTEL_SAMPLE_TEST_LOCK.lock().unwrap();
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
