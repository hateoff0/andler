//! Intel GPU metrics via `i915` sysfs.
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

use super::{read_sysfs_u64_plain, sorted_drm_cards};

/// Documented i915 ABI (kept as a top-level compat path even after the
/// `gt/gt0/` sysfs reorganization — see crate README for the upstream
/// commit reference). Cumulative milliseconds the GPU has spent in the
/// RC6 idle power state since boot.
const INTEL_RC6_RESIDENCY_MS: &str = "device/power/rc6_residency_ms";

/// Previous `(rc6_residency_ms, Instant)` sample for computing GPU load %
/// from a real elapsed-time delta. `Mutex`, not `AtomicU64` — need to store
/// two related values (residency + timestamp) atomically together, not
/// just a single counter; a plain `AtomicU64` for the counter alone would
/// let the residency and the timestamp it was paired with drift apart
/// under concurrent calls.
static PREV_INTEL_SAMPLE: Mutex<Option<(u64, Instant)>> = Mutex::new(None);

/// Finds the first Intel GPU card in sysfs (i915 driver, identified by
/// presence of the `power/rc6_residency_ms` ABI file).
pub(super) fn find_intel_gpu_card() -> Option<PathBuf> {
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
pub(super) fn read_intel_metrics(card_path: &Path) -> ResourceMetrics {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    // tests could observe each other's writes. `INTEL_SAMPLE_TEST_LOCK`
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
}
