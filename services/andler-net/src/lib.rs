//! Network configuration service for ANDLER.
//!
//! Provides bridge and isolated network setup for QEMU virtual machines.

use std::future::Future;
use std::io;

/// Network service errors.
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
    /// Setup bridge mode: attach VM interface to existing bridge.
    fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send;

    /// Teardown bridge mode: remove VM interface from bridge.
    fn teardown_bridge(&self, bridge: &str, vm_iface: &str) -> impl Future<Output = Result<(), NetError>> + Send;

    /// Setup isolated mode: create veth pair and configure isolated network.
    /// Returns (host_veth, vm_veth) interface names.
    fn setup_isolated(&self, vm_iface: &str) -> impl Future<Output = Result<(String, String), NetError>> + Send;

    /// Teardown isolated mode: destroy veth pair.
    fn teardown_isolated(&self, host_veth: &str, vm_veth: &str) -> impl Future<Output = Result<(), NetError>> + Send;
}

/// Default network service implementation using `ip` command.
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
            // Check if bridge exists
            let output = tokio::process::Command::new("ip")
                .args(["link", "show", bridge])
                .output()
                .await
                .map_err(NetError::Io)?;

            if !output.status.success() {
                return Err(NetError::BridgeNotFound(bridge.to_string()));
            }

            // Create TAP interface for VM
            tokio::process::Command::new("ip")
                .args(["link", "add", vm_iface, "type", "tap"])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Attach TAP interface to bridge
            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "master", bridge])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Bring up bridge and TAP interface
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
            // Remove VM interface from bridge
            tokio::process::Command::new("ip")
                .args(["link", "set", vm_iface, "nomaster"])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Delete TAP interface
            tokio::process::Command::new("ip")
                .args(["link", "del", vm_iface])
                .output()
                .await
                .map_err(|e| NetError::CleanupFailed(e.to_string()))?;

            // Bring down VM interface (bridge is left up to avoid affecting other VMs)
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
            // Create TAP interface named 'andler0' for isolated mode
            let tap_iface = vm_iface.to_string();
            
            // Create TAP interface
            tokio::process::Command::new("ip")
                .args(["link", "add", &tap_iface, "type", "tap"])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Bring up TAP interface
            tokio::process::Command::new("ip")
                .args(["link", "set", &tap_iface, "up"])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Apply nftables rules for isolation: allow established/related, drop all else
            // These rules provide stronger isolation than just disabling IP forwarding
            tokio::process::Command::new("nft")
                .args(["add", "rule", "filter", "input", "ct", "state", "established,related", "accept"])
                .output()
                .await?;
            tokio::process::Command::new("nft")
                .args(["add", "rule", "filter", "input", "drop"])
                .output()
                .await?;

            // Return placeholder host_veth (unused) and actual TAP interface
            Ok((String::from("host_veth_placeholder"), tap_iface))
        }
    }

    fn teardown_isolated(&self, _host_veth: &str, vm_veth: &str) -> impl Future<Output = Result<(), NetError>> + Send {
        async move {
            // Bring down TAP interface
            tokio::process::Command::new("ip")
                .args(["link", "set", vm_veth, "down"])
                .output()
                .await
                .map_err(NetError::Io)?;

            // Delete TAP interface
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
