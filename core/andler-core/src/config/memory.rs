use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub size_bytes: u64,

    pub ballooning: bool,

    pub zram: bool,

    pub ksm: bool,
}

impl MemoryConfig {
    pub const GIB: u64 = 1024 * 1024 * 1024;

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
