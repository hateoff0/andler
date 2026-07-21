

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CpuPriority {
    Low,
    #[default]
    Normal,
    High,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuConfig {

    pub cores: u32,

    pub sockets: u32,

    pub threads: u32,

    pub affinity: Option<Vec<usize>>,

    pub priority: CpuPriority,
}

impl CpuConfig {

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
