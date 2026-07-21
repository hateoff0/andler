

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderBackend {

    Venus,

    VirtioGpu,

    VirGl,

    Cpu,

    Passthrough { gpu_pci_id: String },
}

impl RenderBackend {

    pub fn is_implemented(&self) -> bool {
        !matches!(self, RenderBackend::Passthrough { .. })
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuConfig {
    pub render_backend: RenderBackend,

    pub hostmem_bytes: u64,

    pub blob: bool,

    pub gl: bool,
}

impl GpuConfig {
    pub const MIB: u64 = 1024 * 1024;


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
