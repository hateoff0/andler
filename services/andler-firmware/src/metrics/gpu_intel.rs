use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use andler_core::ResourceMetrics;

use super::{read_sysfs_u64_plain, sorted_drm_cards};

const INTEL_RC6_RESIDENCY_MS: &str = "device/power/rc6_residency_ms";

static PREV_INTEL_SAMPLE: Mutex<Option<(u64, Instant)>> = Mutex::new(None);

pub(super) fn find_intel_gpu_card() -> Option<PathBuf> {
    sorted_drm_cards()
        .into_iter()
        .find(|card_path| card_path.join(INTEL_RC6_RESIDENCY_MS).exists())
}

pub(super) fn read_intel_metrics(card_path: &Path) -> ResourceMetrics {
    let current_rc6_ms = read_sysfs_u64_plain(&card_path.join(INTEL_RC6_RESIDENCY_MS));
    let gpu_load_percent = compute_intel_gpu_load(current_rc6_ms);

    ResourceMetrics {
        gpu_load_percent,
        ..ResourceMetrics::default()
    }
}

fn compute_intel_gpu_load(current_rc6_ms: Option<u64>) -> Option<f32> {
    let current = current_rc6_ms?;
    let now = Instant::now();

    let mut guard = PREV_INTEL_SAMPLE.lock().unwrap_or_else(|e| e.into_inner());
    let prev = guard.replace((current, now));

    let (prev_rc6_ms, prev_instant) = prev?;
    let elapsed_ms = now.duration_since(prev_instant).as_millis() as f64;

    intel_gpu_load_from_delta(current.saturating_sub(prev_rc6_ms), elapsed_ms)
}

fn intel_gpu_load_from_delta(idle_delta_ms: u64, elapsed_ms: f64) -> Option<f32> {
    if elapsed_ms <= 0.0 {
        return None;
    }

    let idle_fraction = (idle_delta_ms as f64 / elapsed_ms).clamp(0.0, 1.0);
    let load = ((1.0 - idle_fraction) * 100.0) as f32;
    Some(load.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_intel_gpu_card_does_not_panic() {
        let _ = find_intel_gpu_card();
    }

    #[test]
    fn intel_rc6_residency_path_is_the_real_documented_abi_not_busyiffies() {
        assert_eq!(INTEL_RC6_RESIDENCY_MS, "device/power/rc6_residency_ms");
        assert!(!INTEL_RC6_RESIDENCY_MS.contains("busyiffies"));
        assert!(!INTEL_RC6_RESIDENCY_MS.contains("attrs"));
    }

    #[test]
    fn intel_gpu_load_from_delta_fully_idle_is_zero_load() {
        let result = intel_gpu_load_from_delta(1000, 1000.0);
        assert!((result.unwrap() - 0.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_fully_busy_is_full_load() {
        let result = intel_gpu_load_from_delta(0, 1000.0);
        assert!((result.unwrap() - 100.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_partial_idle() {
        let result = intel_gpu_load_from_delta(300, 1000.0);
        assert!((result.unwrap() - 70.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_clamps_when_idle_exceeds_elapsed() {
        let result = intel_gpu_load_from_delta(1500, 1000.0);
        assert!((result.unwrap() - 0.0).abs() < 0.1);
    }

    #[test]
    fn intel_gpu_load_from_delta_non_positive_elapsed_returns_none() {
        assert!(intel_gpu_load_from_delta(100, 0.0).is_none());
        assert!(intel_gpu_load_from_delta(100, -5.0).is_none());
    }

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
}
