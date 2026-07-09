//! Конфигурация памяти инстанса.
//!
//! Источник истины — исходная референсная конфигурация (ранее описанная в
//! `scripts/start.sh`, который был удалён после миграции всей логики в Rust):
//! `-m 8G` + `-object memory-backend-memfd,size=8G,share=on` (разделяемая память
//! — предпосылка для KSM на хосте, см.
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.2 и §6.1).

use serde::{Deserialize, Serialize};

/// Конфигурация памяти инстанса.
///
/// `ballooning`/`zram`/`ksm` — независимые флаги, хотя на практике
/// осмысленная комбинация для KSM требует `memory-backend-memfd,share=on`
/// на стороне `andler-qemu` независимо от значения `ksm` здесь (KSM —
/// механизм хоста, объединяющий страницы между процессами; этот флаг лишь
/// сигнализирует backend'у, что shared-память нужно включить, чтобы хостовый
/// KSM мог что-то объединять).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Размер RAM инстанса в байтах (не в "G"/"M" — перевод строки вида
    /// `"8G"` в байты происходит на границе CLI/gRPC, здесь только число).
    pub size_bytes: u64,
    /// Включить virtio-balloon — динамическое изменение объёма RAM,
    /// видимого гостю, без перезапуска. Источник метрик при включении —
    /// QMP `query-balloon`, см. §6.1.1 архитектурного плана.
    pub ballooning: bool,
    /// Включить ZRAM внутри гостя (сжатие части RAM вместо использования
    /// диска как swap). Применимо в первую очередь к Android-инстансам.
    pub zram: bool,
    /// Запросить разделяемую (`share=on`) backing memory, необходимую
    /// для того, чтобы Kernel Samepage Merging на хосте могло объединять
    /// одинаковые страницы между несколькими инстансами.
    pub ksm: bool,
}

impl MemoryConfig {
    /// 1 гигабайт в байтах — вспомогательная константа для читаемого
    /// построения конфигураций без магических чисел на каждом сайте вызова.
    pub const GIB: u64 = 1024 * 1024 * 1024;

    /// Конфигурация, соответствующая исходной референсной конфигурации: 8 GiB
    /// RAM, без ballooning/zram, с разделяемой памятью для KSM.
    pub fn reference_default() -> Self {
        MemoryConfig {
            size_bytes: 8 * Self::GIB,
            ballooning: false,
            zram: false,
            ksm: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = MemoryConfig::reference_default();
        assert_eq!(cfg.size_bytes, 8 * MemoryConfig::GIB);
        assert!(!cfg.ballooning);
        assert!(!cfg.zram);
        assert!(cfg.ksm);
    }
}
