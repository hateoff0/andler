

use std::future::Future;
use std::io;


#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("Bridge {0} not found")]
    BridgeNotFound(String),

    #[error("Interface {0} already exists and is in use")]
    InterfaceExists(String),

    #[error("Setup failed: {0}")]
    SetupFailed(String),

    #[error("Cleanup failed: {0}")]
    CleanupFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

pub trait NetworkService {

    fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send;


    fn teardown_bridge(&self, bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send;


    fn setup_isolated(&self, vm_iface: &str) -> impl Future<Output = Result<(String, String), NetError>> + Send;


    fn teardown_isolated(&self, host_veth: &str, vm_veth: &str) -> impl Future<Output = Result<(), NetError>> + Send;
}


pub struct DefaultNetworkService;

impl DefaultNetworkService {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultNetworkService {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkService for DefaultNetworkService {
    fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send {
        async move {
            let output = tokio::process::Command::new("ip")
                .args(["link", "show", bridge])
                .output()
                .await
                .map_err(NetError::Io)?;

            if !output.status.success() {
                return Err(NetError::BridgeNotFound(bridge.to_string()));
            }

            tokio::process::Command::new("ip")
                .args(["link", "add", vm_iface, "type", "tap"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "master", bridge])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "set", bridge, "up"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "up"])
                .output()
                .await
                .map_err(NetError::Io)?;

            Ok(())
        }
    }

    fn teardown_bridge(&self, _bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send {
        async move {
            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "nomaster"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "del", vm_iface])
                .output()
                .await
                .map_err(|e| NetError::CleanupFailed(e.to_string()))?;

            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "down"])
                .output()
                .await
                .map_err(NetError::Io)?;

            Ok(())
        }
    }

    fn setup_isolated(&self, vm_iface: &str) -> impl Future<Output = Result<(String, String), NetError>> + Send {
        async move {
            let tap_iface = vm_iface.to_string();
            
            tokio::process::Command::new("ip")
                .args(["link", "add", &tap_iface, "type", "tap"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "set", &tap_iface, "up"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("nft")
                .args(["add", "rule", "filter", "input", "ct", "state", "established,related", "accept"])
                .output()
                .await?;
            tokio::process::Command::new("nft")
                .args(["add", "rule", "filter", "input", "drop"])
                .output()
                .await?;

            Ok((String::from("host_veth_placeholder"), tap_iface))
        }
    }

    fn teardown_isolated(&self, _host_veth: &str, vm_veth: &str) -> impl Future<Output = Result<(), NetError>> + Send {
        async move {
            tokio::process::Command::new("ip")
                .args(["link", "set", vm_veth, "down"])
                .output()
                .await
                .map_err(NetError::Io)?;

            tokio::process::Command::new("ip")
                .args(["link", "del", vm_veth])
                .output()
                .await
                .map_err(NetError::Io)?;

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_error_display_bridge_not_found() {
        let err = NetError::BridgeNotFound("br0".to_string());
        assert_eq!(err.to_string(), "Bridge br0 not found");
    }

    #[test]
    fn net_error_display_interface_exists() {
        let err = NetError::InterfaceExists("eth0".to_string());
        assert_eq!(err.to_string(), "Interface eth0 already exists and is in use");
    }

    #[test]
    fn net_error_display_setup_failed() {
        let err = NetError::SetupFailed("test error".to_string());
        assert_eq!(err.to_string(), "Setup failed: test error");
    }

    #[test]
    fn net_error_display_cleanup_failed() {
        let err = NetError::CleanupFailed("test error".to_string());
        assert_eq!(err.to_string(), "Cleanup failed: test error");
    }
}
