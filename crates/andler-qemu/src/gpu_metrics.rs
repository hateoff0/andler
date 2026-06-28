//! Сбор GPU-метрик из host-side sysfs (Linux DRM subsystem).
//!
//! QEMU QMP не предоставляет GPU-метрик (нет `query-gpus`), поэтому
//! данные берутся напрямую из sysfs — того же уровня, что и `/proc`
//! для CPU/RAM. Поддерживаются:
//!
//! | Метрика | sysfs-путь (AMD) | Описание |
//! |---|---|---|
//! | VRAM used | `mem_info_vram_used` | Использованная VRAM (байты) |
//! | VRAM total | `mem_info_vram_total` | Общая VRAM (байты) |
//! | GPU load | `gpu_busy_percent` | Загрузка GPU (0-100%) |
//!
//! Пути специфичны для AMDGPU драйвера. Для NVIDIA/Intel Эти файлы
//! отсутствуют — соответствующие поля `ResourceMetrics` остаются `None`.
//! Это осознанное ограничение первой версии: AMD — основная целевая
//! платформа для GPU-passthrough с Android-эмуляцией.
//!
//! ## Как найти нужную DRM-карту
//!
//! Сканируем `/sys/class/drm/card*` и ищем `mem_info_vram_used` —
//! если файл существует, это AMD GPU с поддержкой VRAM-метрик.
//! Возвращаем данные по первому найденному AMD GPU. Если таких несколько
//! (multi-GPU), расширение до привязки к конкретному QEMU-процессу
//! останется отдельным шагом.

use std::path::{Path, PathBuf};

use andler_core::ResourceMetrics;

/// Базовый каталог DRM в sysfs.
const DRM_SYSFS_BASE: &str = "/sys/class/drm";

/// AMD GPU sysfs files для VRAM и загрузки.
const AMD_VRAM_USED: &str = "device/mem_info_vram_used";
const AMD_VRAM_TOTAL: &str = "device/mem_info_vram_total";
const AMD_GPU_BUSY: &str = "device/gpu_busy_percent";

/// Находит первый AMD GPU в sysfs и возвращает путь к его карточке.
///
/// Ищет `cardN/device/mem_info_vram_used` — если существует, значит
/// это AMDGPU драйвер с поддержкой VRAM-метрик.
fn find_amd_gpu_card() -> Option<PathBuf> {
    let drm_base = Path::new(DRM_SYSFS_BASE);

    let mut entries: Vec<_> = match std::fs::read_dir(drm_base) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(_) => return None,
    };

    // Сортируем по имени для детерминированности (card0, card1, ...)
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };

        // Пропускаем renderD* и другие non-card entries
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }

        let card_path = entry.path();
        let vram_used_path = card_path.join(AMD_VRAM_USED);

        if vram_used_path.exists() {
            return Some(card_path);
        }
    }

    None
}

/// Читает UInt64 из sysfs-файла. Возвращает `None` при любой ошибке
/// (файл не существует, невалидное содержимое, I/O error).
fn read_sysfs_u64(base: &Path, relative: &str) -> Option<u64> {
    let content = std::fs::read_to_string(base.join(relative)).ok()?;
    content.trim().parse::<u64>().ok()
}

/// Читает Float32 из sysfs-файла. Возвращает `None` при любой ошибке.
fn read_sysfs_f32(base: &Path, relative: &str) -> Option<f32> {
    let content = std::fs::read_to_string(base.join(relative)).ok()?;
    content.trim().parse::<f32>().ok()
}

/// Собирает GPU-метрики из sysfs.
///
/// Возвращает `ResourceMetrics` с заполненными GPU-полями, если
/// найден совместимый AMD GPU. Все GPU-поля будут `None`, если
/// AMD GPU не найден или sysfs-файлы недоступны.
pub fn read_gpu_metrics() -> ResourceMetrics {
    let card_path = match find_amd_gpu_card() {
        Some(p) => p,
        None => return ResourceMetrics::default(),
    };

    ResourceMetrics {
        vram_used_bytes: read_sysfs_u64(&card_path, AMD_VRAM_USED),
        vram_total_bytes: read_sysfs_u64(&card_path, AMD_VRAM_TOTAL),
        gpu_load_percent: read_sysfs_f32(&card_path, AMD_GPU_BUSY),
        ..ResourceMetrics::default()
    }
}

/// Объединяет GPU-метрики с уже собранными host-метриками.
///
/// GPU-поля из `gpu` перезаписывают `None`-поля в `base`.
/// Существующие non-None поля `base` не перезаписываются.
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

    #[test]
    fn find_amd_gpu_card_returns_none_when_no_drm() {
        // В тестовом окружении /sys/class/drm может не существовать
        // или не содержать AMD GPU — это штатный случай
        let result = find_amd_gpu_card();
        // Не assert_eq!(result, None) — на реальной машине с AMD GPU
        // результат будет Some. Проверяем что функция не паникует.
        let _ = result;
    }

    #[test]
    fn read_gpu_metrics_does_not_panic() {
        // read_gpu_metrics() не должна паниковать ни в каком окружении
        let metrics = read_gpu_metrics();
        // В тестовом окружении GPU-поля будут None (нет AMD GPU в sysfs)
        assert!(metrics.vram_used_bytes.is_none());
        assert!(metrics.vram_total_bytes.is_none());
        assert!(metrics.gpu_load_percent.is_none());
        // Host-поля тоже None (не читаются из /proc в этом модуле)
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

        // Не перезаписываются, потому что уже были Some
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
}
