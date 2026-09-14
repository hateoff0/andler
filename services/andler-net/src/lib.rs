use std::io;

use async_trait::async_trait;

mod isolated;

pub use isolated::{IsolatedNet, MAX_IFACE_LEN};

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

    #[error("Isolation leaked: {0}")]
    IsolationLeaked(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

#[async_trait]
pub trait NetworkService {
    async fn setup_bridge(&self, bridge: &str, vm_iface: &str) -> Result<(), NetError>;

    async fn teardown_bridge(&self, bridge: &str, vm_iface: &str) -> Result<(), NetError>;

    async fn setup_isolated(&self, vm_iface: &str) -> Result<IsolatedNet, NetError>;

    async fn teardown_isolated(&self, net: &IsolatedNet) -> Result<(), NetError>;
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

        run_ip(&["tuntap", "add", "dev", vm_iface, "mode", "tap"]).await?;
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

    // Isolated mode used to create a plain host tap and add a *global*, unscoped
    // `nft add rule filter input drop` to the host's firewall — which dropped ALL
    // inbound host traffic (SSH included) the moment any VM used this mode, while
    // providing no actual isolation at all (the returned "host_veth" was a literal
    // placeholder string, not a real interface). That combination — breaks the host,
    // isolates nothing — is worse than not implementing the mode, so it was removed
    // rather than patched.
    //
    // The mode is now real, and the host stays untouched: the guest's own QEMU runs
    // inside a user + network namespace it creates at spawn time (`IsolatedNet::launcher`,
    // see the crate README). That namespace holds loopback and the instance's tap and
    // nothing else — no veth, no bridge, no route — so there is no path to any host
    // network, and no host-side interface that could leak or need cleaning up. QMP and
    // the guest-agent chardev are UNIX sockets on the shared filesystem, so host-side
    // control survives the network namespace. This function is the gate: it refuses,
    // with the reason, before QEMU is touched when the host cannot provide that namespace.
    async fn setup_isolated(&self, vm_iface: &str) -> Result<IsolatedNet, NetError> {
        let net = IsolatedNet::new(vm_iface);
        isolated::preflight(&net).await?;
        Ok(net)
    }

    async fn teardown_isolated(&self, net: &IsolatedNet) -> Result<(), NetError> {
        // Nothing to undo: the kernel destroys the namespace — and with it the tap —
        // when the guest process exits. The only way an interface of this name can be
        // on the host is that it was created outside the namespace, which would mean
        // the guest ran unisolated; remove it and say so.
        let tap = net.tap_iface();
        let shown = tokio::process::Command::new("ip")
            .args(["link", "show", tap])
            .output()
            .await
            .map_err(NetError::Io)?;

        if !shown.status.success() {
            return Ok(());
        }

        run_ip(&["link", "del", tap]).await?;
        Err(NetError::IsolationLeaked(format!(
            "the tap `{tap}` was on the host instead of inside the guest's namespace: removed it, \
             but that guest did not run isolated"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

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

    #[test]
    fn net_error_display_isolation_leaked() {
        let err = NetError::IsolationLeaked("tap on the host".to_string());
        assert_eq!(err.to_string(), "Isolation leaked: tap on the host");
    }

    fn sysctl_root(name: &str, knobs: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("andler-net-sysctl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (relative, contents) in knobs {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("knob path has a parent"))
                .expect("create fake sysctl dir");
            std::fs::write(path, contents).expect("write fake sysctl knob");
        }
        root
    }

    fn drop_sysctl_root(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    fn binary_available(name: &str) -> bool {
        std::process::Command::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn tap_name(prefix: &str) -> String {
        let tap = format!("{prefix}{}", std::process::id() % 100_000);
        assert!(
            tap.len() <= MAX_IFACE_LEN,
            "test tap name must fit IFNAMSIZ"
        );
        tap
    }

    async fn connected_or_skip(service: &DefaultNetworkService, tap: &str) -> Option<IsolatedNet> {
        match service.setup_isolated(tap).await {
            Ok(net) => Some(net),
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("unprivileged_userns_clone")
                        || message.contains("max_user_namespaces"),
                    "a refusal must name the knob that blocks isolated mode: {message}"
                );
                eprintln!("isolated network mode is unavailable on this host: {message}");
                None
            }
        }
    }

    #[test]
    fn userns_disabled_by_distro_knob_is_reported() {
        let root = sysctl_root(
            "distro-knob",
            &[("kernel/unprivileged_userns_clone", "0\n")],
        );
        let reason = isolated::user_namespaces_disabled(&root).expect("knob 0 must be reported");
        assert!(
            reason.contains("kernel.unprivileged_userns_clone=1"),
            "{reason}"
        );
        assert!(reason.contains("network.mode"), "{reason}");
        drop_sysctl_root(&root);
    }

    #[test]
    fn userns_disabled_by_upstream_limit_is_reported() {
        let root = sysctl_root("upstream-knob", &[("user/max_user_namespaces", "0\n")]);
        assert!(isolated::user_namespaces_disabled(&root).is_some());
        drop_sysctl_root(&root);
    }

    #[test]
    fn enabled_knobs_are_not_reported_as_disabled() {
        let root = sysctl_root(
            "enabled-knobs",
            &[
                ("kernel/unprivileged_userns_clone", "1\n"),
                ("user/max_user_namespaces", "255875\n"),
            ],
        );
        assert_eq!(isolated::user_namespaces_disabled(&root), None);
        drop_sysctl_root(&root);
    }

    #[test]
    fn missing_knobs_leave_the_decision_to_the_probe() {
        let root = sysctl_root("no-knobs", &[]);
        assert_eq!(isolated::user_namespaces_disabled(&root), None);
        drop_sysctl_root(&root);
    }

    #[tokio::test]
    async fn setup_isolated_refuses_with_an_actionable_reason_when_user_namespaces_are_disabled() {
        let root = sysctl_root("refusal", &[("kernel/unprivileged_userns_clone", "0\n")]);
        let net = IsolatedNet::new("andler-test0");
        let err = isolated::preflight_with_sysctl_root(&root, &net)
            .await
            .expect_err("a disabled user-namespace knob must refuse isolated setup");
        let message = err.to_string();
        assert!(
            message.contains("kernel.unprivileged_userns_clone=1"),
            "{message}"
        );
        assert!(
            message.contains("nat") && message.contains("bridge"),
            "{message}"
        );
        drop_sysctl_root(&root);
    }

    #[tokio::test]
    async fn setup_isolated_rejects_a_tap_name_the_kernel_cannot_take() {
        let service = DefaultNetworkService;
        let err = service
            .setup_isolated("andler-tap-name-that-is-too-long")
            .await
            .expect_err("names above IFNAMSIZ must be refused");
        let message = err.to_string();
        assert!(message.contains("IFNAMSIZ"), "{message}");
        assert!(message.contains("15"), "{message}");
    }

    #[tokio::test]
    async fn isolated_namespace_holds_only_loopback_and_the_tap() {
        let service = DefaultNetworkService;
        let tap = tap_name("andler-t");
        let Some(net) = connected_or_skip(&service, &tap).await else {
            return;
        };

        let run = isolated::run_probe(&net)
            .await
            .expect("the probe must spawn");
        let view = run.view.expect("the probe namespace must be readable");
        let host = isolated::NamespaceView::current().expect("the host namespace");

        assert_ne!(
            view.netns, host.netns,
            "the guest must run in its own network namespace"
        );
        assert!(
            view.has_iface(&tap),
            "the tap must exist inside the namespace: {:?}",
            view.ifaces
        );
        assert_eq!(
            view.ifaces.len(),
            2,
            "only loopback and the tap may exist inside: {:?}",
            view.ifaces
        );
        assert!(view.ifaces.contains(&"lo".to_string()), "{:?}", view.ifaces);
        assert_eq!(view.v4_routes, 0, "the namespace must have no IPv4 route");
        assert!(view.leaks(&tap).is_empty(), "{:?}", view.leaks(&tap));
        println!(
            "isolated namespace for `{tap}`: netns {} (host {}), interfaces {:?}, IPv4 routes {}",
            view.netns, host.netns, view.ifaces, view.v4_routes
        );

        let on_host = std::process::Command::new("ip")
            .args(["link", "show", &tap])
            .output()
            .expect("ip must run");
        assert!(
            !on_host.status.success(),
            "the tap must never be visible on the host"
        );

        let after = isolated::NamespaceView::read(run.pid);
        let still_there = after.as_ref().is_some_and(|v| v.has_iface(&tap));
        assert!(
            !still_there,
            "the namespace must die with the guest process"
        );

        service
            .teardown_isolated(&net)
            .await
            .expect("teardown must succeed");
        service
            .teardown_isolated(&net)
            .await
            .expect("teardown must be idempotent");
    }

    #[tokio::test]
    async fn isolated_guest_keeps_host_side_control_over_qmp() {
        if !binary_available("qemu-system-x86_64") {
            eprintln!("skipping: qemu-system-x86_64 is not in PATH");
            return;
        }
        let service = DefaultNetworkService;
        let tap = tap_name("andler-q");
        let Some(net) = connected_or_skip(&service, &tap).await else {
            return;
        };

        let dir = std::env::temp_dir().join(format!("andler-net-qmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        let qmp = dir.join("qmp.sock");

        let launcher = net.launcher();
        let mut child = tokio::process::Command::new(&launcher[0])
            .args(&launcher[1..])
            .arg("qemu-system-x86_64")
            .args([
                "-name",
                "andler-net-test,process=andler-net-test",
                "-nodefaults",
                "-M",
                "q35",
                "-m",
                "128",
                "-S",
                "-display",
                "none",
                "-monitor",
                "none",
                "-qmp",
                &format!("unix:{},server,nowait", qmp.display()),
                "-netdev",
                &format!("tap,id=net0,ifname={tap},script=no,downscript=no"),
                "-device",
                "virtio-net-pci,netdev=net0",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the launcher must start qemu");
        let pid = child.id().expect("spawned qemu has a pid");

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&qmp).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(
                matches!(child.try_wait(), Ok(None)),
                "qemu must stay up inside the namespace"
            );
        }

        let view =
            isolated::NamespaceView::read(pid).expect("the guest namespace must be readable");
        assert!(view.has_iface(&tap), "{:?}", view.ifaces);
        assert!(view.leaks(&tap).is_empty(), "{:?}", view.leaks(&tap));

        let mut stream = std::os::unix::net::UnixStream::connect(&qmp)
            .expect("the host must reach the QMP socket of a guest inside the namespace");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        let greeting = read_qmp(&mut stream);
        assert!(greeting.contains("\"QMP\""), "{greeting}");
        stream
            .write_all(br#"{"execute":"qmp_capabilities"}"#)
            .expect("send qmp_capabilities");
        assert!(read_qmp(&mut stream).contains("return"));

        let _ = child.start_kill();
        let _ = child.wait().await;

        let after = isolated::NamespaceView::read(pid);
        assert!(
            after.is_none_or(|v| !v.has_iface(&tap)),
            "the namespace must die with the guest"
        );
        service
            .teardown_isolated(&net)
            .await
            .expect("teardown must succeed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read_qmp(stream: &mut std::os::unix::net::UnixStream) -> String {
        let mut buffer = [0u8; 4096];
        let read = stream.read(&mut buffer).expect("read qmp reply");
        String::from_utf8_lossy(&buffer[..read]).into_owned()
    }
}
