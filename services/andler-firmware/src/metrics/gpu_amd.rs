

use std::path::{Path, PathBuf};

use andler_core::ResourceMetrics;

use super::{read_sysfs_f32, read_sysfs_u64, sorted_drm_cards};

const AMD_VRAM_USED: &str = "device/mem_info_vram_used";
const AMD_VRAM_TOTAL: &str = "device/mem_info_vram_total";
const AMD_GPU_BUSY: &str = "device/gpu_busy_percent";


pub(super) fn find_amd_gpu_card() -> Option<PathBuf> {
    for card_path in sorted_drm_cards() {
        if card_path.join(AMD_VRAM_USED).exists() {
            return Some(card_path);
        }
    }
    None
}


pub(super) fn read_amd_metrics(card_path: &Path) -> ResourceMetrics {
    ResourceMetrics {
        vram_used_bytes: read_sysfs_u64(card_path, AMD_VRAM_USED),
        vram_total_bytes: read_sysfs_u64(card_path, AMD_VRAM_TOTAL),
        gpu_load_percent: read_sysfs_f32(card_path, AMD_GPU_BUSY),
        ..ResourceMetrics::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_amd_gpu_card_does_not_panic() {
        let _ = find_amd_gpu_card();
    }
}
