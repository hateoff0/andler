use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use crate::NetError;

pub const MAX_IFACE_LEN: usize = 15;

const SYSCTL_ROOT: &str = "/proc/sys";
const CLONE_KNOB: &str = "kernel/unprivileged_userns_clone";
const MAX_NAMESPACES_KNOB: &str = "user/max_user_namespaces";
const ISOLATION_TOOL: &str = "unshare";
const SHELL: &str = "sh";
const RECIPE_ARGV0: &str = "andler-qemu";
const PROBE_HOLD: &str = "sleep 5";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_POLL: Duration = Duration::from_millis(20);

const TAP_RECIPE: &str = concat!(
    "set -eu\n",
    "ip link set lo up\n",
    "ip tuntap add dev \"$1\" mode tap\n",
    "ip link set \"$1\" up\n",
    "shift\n",
    "exec \"$@\"\n"
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedNet {
    tap_iface: String,
    launcher: Vec<String>,
}

impl IsolatedNet {
    pub fn new(tap_iface: impl Into<String>) -> Self {
        let tap_iface = tap_iface.into();
        let launcher = vec![
            ISOLATION_TOOL.to_string(),
            "--user".to_string(),
            "--map-root-user".to_string(),
            "--net".to_string(),
            "--".to_string(),
            SHELL.to_string(),
            "-c".to_string(),
            TAP_RECIPE.to_string(),
            RECIPE_ARGV0.to_string(),
            tap_iface.clone(),
        ];
        Self {
            tap_iface,
            launcher,
        }
    }

    pub fn tap_iface(&self) -> &str {
        &self.tap_iface
    }

    pub fn launcher(&self) -> &[String] {
        &self.launcher
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamespaceView {
    pub netns: String,
    pub ifaces: Vec<String>,
    pub v4_routes: usize,
}

impl NamespaceView {
    pub(crate) fn read(pid: u32) -> Option<Self> {
        let netns = std::fs::read_link(format!("/proc/{pid}/ns/net"))
            .ok()?
            .to_string_lossy()
            .into_owned();
        let dev = std::fs::read_to_string(format!("/proc/{pid}/net/dev")).ok()?;
        let ifaces = dev
            .lines()
            .skip(2)
            .filter_map(|line| line.split(':').next())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        let v4_routes = std::fs::read_to_string(format!("/proc/{pid}/net/route"))
            .map(|table| table.lines().count().saturating_sub(1))
            .unwrap_or(0);
        Some(Self {
            netns,
            ifaces,
            v4_routes,
        })
    }

    pub(crate) fn current() -> Option<Self> {
        Self::read(std::process::id())
    }

    pub(crate) fn has_iface(&self, name: &str) -> bool {
        self.ifaces.iter().any(|iface| iface == name)
    }

    pub(crate) fn leaks(&self, tap: &str) -> Vec<String> {
        let mut leaks = Vec::new();
        for iface in &self.ifaces {
            if iface != "lo" && iface != tap {
                leaks.push(format!("it carries the extra interface `{iface}`"));
            }
        }
        if self.v4_routes > 0 {
            leaks.push(format!(
                "it has {} IPv4 route table entries",
                self.v4_routes
            ));
        }
        leaks
    }
}

pub(crate) struct ProbeRun {
    pub pid: u32,
    pub view: Option<NamespaceView>,
    pub stderr: String,
}

pub(crate) async fn preflight(net: &IsolatedNet) -> Result<(), NetError> {
    preflight_with_sysctl_root(Path::new(SYSCTL_ROOT), net).await
}

pub(crate) async fn preflight_with_sysctl_root(
    sysctl_root: &Path,
    net: &IsolatedNet,
) -> Result<(), NetError> {
    let tap = net.tap_iface();
    if tap.is_empty() || tap.len() > MAX_IFACE_LEN {
        return Err(NetError::SetupFailed(format!(
            "isolated network mode needs a tap interface name of 1..={MAX_IFACE_LEN} characters \
             (IFNAMSIZ), got `{tap}` ({})",
            tap.len()
        )));
    }
    if let Some(reason) = user_namespaces_disabled(sysctl_root) {
        return Err(NetError::SetupFailed(reason));
    }

    let run = run_probe(net).await?;
    let host_netns = NamespaceView::current().map(|view| view.netns);
    let inside = match &run.view {
        Some(view) if Some(&view.netns) != host_netns.as_ref() && view.has_iface(tap) => view,
        _ => return Err(namespace_refused(run.pid, &run.stderr)),
    };

    let leaks = inside.leaks(tap);
    if !leaks.is_empty() {
        return Err(NetError::SetupFailed(format!(
            "isolated network mode started a namespace that is not isolated: {}. This is a bug in \
             andler-net, not a host misconfiguration — report it rather than running the guest",
            leaks.join("; ")
        )));
    }
    Ok(())
}

pub(crate) async fn run_probe(net: &IsolatedNet) -> Result<ProbeRun, NetError> {
    let launcher = net.launcher();
    let mut child = Command::new(&launcher[0])
        .args(&launcher[1..])
        .args([SHELL, "-c", PROBE_HOLD])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            NetError::SetupFailed(format!(
                "isolated network mode cannot run `{}`: {error} — install util-linux (unshare) or \
                 use network.mode = \"nat\" / \"bridge\"",
                launcher[0]
            ))
        })?;
    let pid = child.id().unwrap_or(0);

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut view = NamespaceView::read(pid);
    loop {
        if view
            .as_ref()
            .is_some_and(|candidate| candidate.has_iface(net.tap_iface()))
        {
            break;
        }
        if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(PROBE_POLL).await;
        view = NamespaceView::read(pid);
    }

    let stderr = stop(child).await;
    Ok(ProbeRun { pid, view, stderr })
}

async fn stop(mut child: Child) -> String {
    let _ = child.start_kill();
    match child.wait_with_output().await {
        Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn namespace_refused(pid: u32, stderr: &str) -> NetError {
    let detail = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("the unprivileged user and network namespace was refused");
    NetError::SetupFailed(format!(
        "isolated network mode needs an unprivileged user and network namespace, and this host \
         did not provide one (probe process {pid}): {detail}. Enable unprivileged user \
         namespaces (sudo sysctl kernel.unprivileged_userns_clone=1 on Debian/Ubuntu-style \
         kernels, sudo sysctl -w user.max_user_namespaces=10000 for the upstream limit) and make \
         sure the `tun` module and /dev/net/tun are available, or use network.mode = \"nat\" / \
         \"bridge\"."
    ))
}

pub(crate) fn user_namespaces_disabled(sysctl_root: &Path) -> Option<String> {
    let clone_knob = read_enabled_knob(sysctl_root, CLONE_KNOB);
    let max_knob = read_enabled_knob(sysctl_root, MAX_NAMESPACES_KNOB);
    if clone_knob != Some(false) && max_knob != Some(false) {
        return None;
    }
    Some(format!(
        "isolated network mode needs unprivileged user namespaces ({CLONE_KNOB} or \
         {MAX_NAMESPACES_KNOB}), and this kernel has them disabled: enable them with `sudo \
         sysctl kernel.unprivileged_userns_clone=1` (Debian/Ubuntu-style kernels) or `sudo \
         sysctl -w user.max_user_namespaces=10000` (upstream limit), or use network.mode = \
         \"nat\" / \"bridge\"."
    ))
}

fn read_enabled_knob(sysctl_root: &Path, relative: &str) -> Option<bool> {
    let value = std::fs::read_to_string(sysctl_root.join(relative)).ok()?;
    value.trim().parse::<u64>().ok().map(|limit| limit != 0)
}
