use std::io;

use async_trait::async_trait;

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

#[async_trait]
pub trait NetworkService {
    async fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> Result<(), NetError>;

    async fn teardown_bridge(&self, bridge: &str, vm_iface: &str) -> Result<(), NetError>;

    async fn setup_isolated(&self, vm_iface: &str) -> Result<(String, String), NetError>;

    async fn teardown_isolated(&self, host_veth: &str, vm_veth: &str) -> Result<(), NetError>;
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

/// Runs `ip <args>`, treating a non-zero exit as a real error — not just a failure
/// to spawn the process. `ip` reports most failures (permission denied, interface
/// already exists, no such device, ...) by exiting non-zero while spawning fine, so
/// checking only the spawn result silently ignores exactly the failures that matter.
async fn run_ip(args: &[&str]) -> Result<(), NetError> {
    let output = tokio::process::Command::new("ip")
        .args(args)
        .output()
        .await
        .map_err(NetError::Io)?;

    if !output.status.success() {
        return Err(NetError::SetupFailed(format!(
            "ip {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[async_trait]
impl NetworkService for DefaultNetworkService {
    async fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> Result<(), NetError> {
        let output = tokio::process::Command::new("ip")
            .args(["link", "show", bridge])
            .output()
            .await
            .map_err(NetError::Io)?;

        if !output.status.success() {
            return Err(NetError::BridgeNotFound(bridge.to_string()));
        }

        run_ip(&["link", "add", vm_iface, "type", "tap"]).await?;
        run_ip(&["link", "set", vm_iface, "master", bridge]).await?;
        run_ip(&["link", "set", bridge, "up"]).await?;
        run_ip(&["link", "set", vm_iface, "up"]).await?;

        Ok(())
    }

    async fn teardown_bridge(&self, _bridge: &str, vm_iface: &str) -> Result<(), NetError> {
        run_ip(&["link", "set", vm_iface, "nomaster"]).await?;

        // Bring the link down *before* deleting it, not after — deleting first
        // and then trying to set the (now-gone) interface down was a
        // use-after-delete: that last step could never do anything but fail.
        run_ip(&["link", "set", vm_iface, "down"]).await?;

        tokio::process::Command::new("ip")
            .args(["link", "del", vm_iface])
            .output()
            .await
            .map_err(|e| NetError::CleanupFailed(e.to_string()))?;

        Ok(())
    }

    async fn setup_isolated(&self, _vm_iface: &str) -> Result<(String, String), NetError> {
        // This used to create a plain tap interface and add a *global*, unscoped
        // `nft add rule filter input drop` to the host's firewall — which drops
        // ALL inbound host traffic (SSH included) the moment any VM used this
        // mode, while providing no actual isolation at all (the returned
        // "host_veth" was a literal placeholder string, not a real interface).
        // That combination — breaks the host, isolates nothing — is worse than
        // just not implementing this yet.
        //
        // Real isolation needs a network namespace + veth pair *and* the VM's
        // own QEMU process spawned inside that namespace (`ip netns exec`) —
        // spawning happens in a different module (process.rs) that has no
        // namespace awareness today. Wiring that up correctly needs testing on
        // real hardware with real network privileges, which isn't available
        // here — failing loudly and honestly beats silently pretending this
        // works. Use NAT or Bridge mode until this is implemented for real.
        Err(NetError::SetupFailed(
            "isolated network mode is not implemented yet — it would need to \
             spawn the VM inside a network namespace, which isn't wired up. \
             Use NAT or Bridge mode instead."
                .to_string(),
        ))
    }

    async fn teardown_isolated(&self, _host_veth: &str, vm_veth: &str) -> Result<(), NetError> {
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
        assert_eq!(
            err.to_string(),
            "Interface eth0 already exists and is in use"
        );
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

    #[tokio::test]
    async fn setup_isolated_fails_cleanly_instead_of_silently_doing_nothing() {
        // Regression test: this used to add a global, unscoped `nft ... drop` rule to
        // the host's input chain (breaking all inbound host traffic) while returning
        // a placeholder string instead of any real isolation. It must now fail
        // loudly with no side effects rather than pretend to succeed.
        let service = DefaultNetworkService;
        let err = service.setup_isolated("andler-test0").await.unwrap_err();
        assert!(matches!(err, NetError::SetupFailed(_)));
    }
}
