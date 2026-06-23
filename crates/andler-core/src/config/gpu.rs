//! Конфигурация рендеринга/GPU инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-device virtio-gpu-gl,hostmem=4096M,blob=true,venus=true` +
//! `-display sdl,gl=on,show-cursor=off`. См.
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.3, §4.1.

use serde::{Deserialize, Serialize};

/// Варианты рендеринга.
///
/// `Passthrough` зарезервирован в enum, чтобы не ломать публичный API при
/// будущем добавлении VFIO GPU passthrough, но **не реализуется на текущем
/// этапе**: любой backend, получивший этот вариант, обязан вернуть
/// `BackendError::InvalidConfig` (если решение принято до `spawn`, через
/// `HypervisorBackend::supported_render_backends`) или
/// `BackendError::NotImplemented` (если конкретный шаг сборки cmdline не
/// реализован). См. §2.3 архитектурного плана — причина отказа: VFIO
/// требует IOMMU-группировки и отдельной настройки хоста, это отдельная
/// большая задача, не блокирующая MVP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderBackend {
    /// `virtio-gpu-gl,...,venus=true` — Vulkan через Venus, максимальная
    /// производительность для современных игр/приложений.
    Venus,
    /// `virtio-gpu-pci` — универсальный VirtIO-GPU без Venus-контекста.
    VirtioGpu,
    /// VirGL-рендеринг (OpenGL поверх virtio-gpu без Vulkan/Venus) —
    /// промежуточный вариант для GPU/драйверов без поддержки Venus.
    VirGl,
    /// `-vga std` — программный рендеринг без аппаратного ускорения.
    /// Самый совместимый, но непригодный для игр вариант.
    Cpu,
    /// Зарезервировано на будущее. См. документацию enum выше — не
    /// реализуется сейчас.
    Passthrough { gpu_pci_id: String },
}

impl RenderBackend {
    /// `true`, если вариант зарезервирован, но не реализуется на текущем
    /// этапе разработки. Используется в `andler-qemu` и в реестре
    /// backend'ов `andler-daemon` для единообразной ранней проверки перед
    /// `spawn`, вместо разбросанных по коду `match`-проверок на конкретный
    /// вариант.
    pub fn is_implemented(&self) -> bool {
        !matches!(self, RenderBackend::Passthrough { .. })
    }
}

/// Конфигурация GPU/рендеринга инстанса.
///
/// Отдельно от `RenderBackend`, так как один и тот же backend (например,
/// `Venus`) может запускаться с разным объёмом выделенной видеопамяти —
/// `hostmem` в `start.sh` это параметр устройства, а не выбор варианта
/// рендеринга.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuConfig {
    pub render_backend: RenderBackend,
    /// Объём памяти хоста, выделенной для GPU-устройства, в байтах
    /// (`hostmem` в `start.sh`, там `4096M`). Не имеет смысла для
    /// `RenderBackend::Cpu`.
    pub hostmem_bytes: u64,
    /// `blob=true` — поддержка blob-модели, требуется для Venus.
    pub blob: bool,
    /// `gl=on` на стороне `-display` — включение OpenGL для совместимости
    /// отображения с выбранным render backend'ом.
    pub gl: bool,
}

impl GpuConfig {
    pub const MIB: u64 = 1024 * 1024;

    /// Конфигурация, соответствующая `start.sh`: Venus, 4096 MiB hostmem,
    /// blob и gl включены.
    pub fn reference_default() -> Self {
        GpuConfig {
            render_backend: RenderBackend::Venus,
            hostmem_bytes: 4096 * Self::MIB,
            blob: true,
            gl: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_is_not_implemented() {
        let backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        assert!(!backend.is_implemented());
    }

    #[test]
    fn venus_and_friends_are_implemented() {
        for backend in [
            RenderBackend::Venus,
            RenderBackend::VirtioGpu,
            RenderBackend::VirGl,
            RenderBackend::Cpu,
        ] {
            assert!(backend.is_implemented());
        }
    }

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = GpuConfig::reference_default();
        assert_eq!(cfg.render_backend, RenderBackend::Venus);
        assert_eq!(cfg.hostmem_bytes, 4096 * GpuConfig::MIB);
        assert!(cfg.blob);
        assert!(cfg.gl);
    }
}
