use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub size_bytes: u64,

    #[serde(default)]
    pub ballooning: bool,

    #[serde(default)]
    pub zram: bool,

    #[serde(default)]
    pub ksm: bool,

    #[serde(default)]
    pub overcommit_mem_lock: bool,

    #[serde(default)]
    pub hugepages: bool,
}

impl MemoryConfig {
    pub const GIB: u64 = 1024 * 1024 * 1024;

    pub fn reference_default() -> Self {
        MemoryConfig {
            size_bytes: 8 * Self::GIB,
            ballooning: false,
            zram: false,
            ksm: true,
            overcommit_mem_lock: false,
            hugepages: false,
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
        assert!(!cfg.overcommit_mem_lock);
        assert!(!cfg.hugepages);
    }

    #[test]
    fn overcommit_mem_lock_defaults_to_false_when_omitted() {
        let minimal = "size_bytes = 8589934592";
        let cfg: MemoryConfig = toml::from_str(minimal).unwrap();
        assert!(!cfg.ballooning);
        assert!(!cfg.zram);
        assert!(!cfg.ksm);
        assert!(!cfg.overcommit_mem_lock);
        assert!(!cfg.hugepages);
    }

    #[test]
    fn overcommit_mem_lock_parses_true() {
        let toml = "size_bytes = 8589934592\novercommit_mem_lock = true";
        let cfg: MemoryConfig = toml::from_str(toml).unwrap();
        assert!(cfg.overcommit_mem_lock);
    }

    #[test]
    fn hugepages_defaults_to_false_when_omitted() {
        let minimal = "size_bytes = 8589934592";
        let cfg: MemoryConfig = toml::from_str(minimal).unwrap();
        assert!(!cfg.hugepages);
    }

    #[test]
    fn hugepages_parses_true() {
        let toml = "size_bytes = 8589934592\nhugepages = true";
        let cfg: MemoryConfig = toml::from_str(toml).unwrap();
        assert!(cfg.hugepages);
    }
}
