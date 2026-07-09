//! Конфигурация CPU инстанса.
//!
//! Источник истины по дефолтным значениям — исходная референсная конфигурация (ранее описанная в
//! `scripts/start.sh`, который был удалён после миграции всей логики в Rust):
//! `-smp cpus=4,sockets=1,dies=1,cores=4,threads=1`,
//! `-cpu host,kvm=on,+topoext,migratable=no`. См.
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.2/§2.4.

use serde::{Deserialize, Serialize};

/// Приоритет процесса QEMU на хосте (влияет на `nice`/`ionice`-подобные
/// настройки при spawn в `andler-qemu`; сам enum здесь, в `andler-core`,
/// так как это часть декларативной конфигурации, а не деталь backend'а).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CpuPriority {
    Low,
    #[default]
    Normal,
    High,
}

/// Конфигурация CPU инстанса.
///
/// `sockets * cores * threads` должно соответствовать `cores` в смысле
/// общего количества виртуальных CPU, передаваемых в `-smp`; конкретная
/// арифметика и валидация (например, что произведение равно общему числу
/// vCPU) — задача `andler-qemu::cmdline`, а не этого типа. Здесь —
/// только декларативные данные, без проверок на момент конструирования.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuConfig {
    /// Общее количество виртуальных ядер (`-smp cpus=N`).
    pub cores: u32,
    /// Количество "сокетов" в топологии (`-smp sockets=N`). На практике
    /// почти всегда `1` для десктопных инстансов — несколько сокетов имеет
    /// смысл только при эмуляции NUMA, что вне скоупа MVP.
    /// Количество потоков на ядро (`-smp threads=N`). В исходной референсной конфигурации
    /// всегда `1` (SMT не эмулируется отдельно от физических ядер хоста).

    pub threads: u32,
    /// Привязка виртуальных CPU к конкретным физическим ядрам хоста
    /// (`taskset`-подобное поведение на стороне `andler-qemu`). `None` —
    /// без привязки, планировщик хоста решает сам (поведение исходной референсной конфигурации
    /// по умолчанию).
    pub affinity: Option<Vec<usize>>,
    /// Приоритет процесса на хосте.
    pub priority: CpuPriority,
}

impl CpuConfig {
    /// Конфигурация, соответствующая исходной референсной конфигурации: 4 ядра, 1 сокет, 1 поток
    /// на ядро, без привязки к конкретным физическим ядрам, обычный
    /// приоритет.
    pub fn reference_default() -> Self {
        CpuConfig {
            cores: 4,
            sockets: 1,
            threads: 1,
            affinity: None,
            priority: CpuPriority::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = CpuConfig::reference_default();
        assert_eq!(cfg.cores, 4);
        assert_eq!(cfg.sockets, 1);
        assert_eq!(cfg.threads, 1);
        assert_eq!(cfg.affinity, None);
        assert_eq!(cfg.priority, CpuPriority::Normal);
    }
}
