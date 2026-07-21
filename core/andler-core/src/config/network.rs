

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatBackend {

    Slirp,

    Passt,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkMode {

    Nat,

    Bridge { interface: String },

    Isolated,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub mode: NetworkMode,

    pub device_model: String,

    #[serde(default = "default_nat_backend")]
    pub nat_backend: NatBackend,
}

fn default_nat_backend() -> NatBackend {
    NatBackend::Slirp
}

impl NetworkConfig {

    pub fn reference_default() -> Self {
        NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Slirp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = NetworkConfig::reference_default();
        assert_eq!(cfg.mode, NetworkMode::Nat);
        assert_eq!(cfg.device_model, "virtio-net-pci");
        assert_eq!(cfg.nat_backend, NatBackend::Slirp);
    }

    #[test]
    fn nat_backend_deserializes_with_default_when_missing() {
        let json = r#"{"mode":"Nat","device_model":"virtio-net-pci"}"#;
        let cfg: NetworkConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.nat_backend, NatBackend::Slirp);
    }
}
