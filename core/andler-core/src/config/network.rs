use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NatBackend {
    Slirp,

    Passt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
}

/// A host→guest TCP or UDP port forwarding entry (`-netdev hostfwd=`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForward {
    pub protocol: PortForwardProtocol,

    pub host_port: u16,

    pub guest_port: u16,

    #[serde(default)]
    pub host_address: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortForwardProtocol {
    Tcp,

    Udp,
}

impl NetworkConfig {
    pub fn reference_default() -> Self {
        NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Slirp,
            port_forwards: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for fwd in &self.port_forwards {
            if fwd.host_port == 0 {
                return Err(format!(
                    "network.port_forwards[{}]: host_port must be 1..=65535 (got 0)",
                    fwd.guest_port
                ));
            }
            if fwd.guest_port == 0 {
                return Err(format!(
                    "network.port_forwards[{}]: guest_port must be 1..=65535 (got 0)",
                    fwd.host_port
                ));
            }
        }
        if !self.port_forwards.is_empty() {
            match &self.mode {
                NetworkMode::Nat => {}
                NetworkMode::Bridge { .. } | NetworkMode::Isolated => {
                    return Err(
                        "network.port_forwards are only supported with mode = nat".to_string()
                    );
                }
            }
        }
        Ok(())
    }
}

fn default_nat_backend() -> NatBackend {
    NatBackend::Slirp
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
        let json = r#"{"mode":"nat","device_model":"virtio-net-pci"}"#;
        let cfg: NetworkConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.nat_backend, NatBackend::Slirp);
    }
}
