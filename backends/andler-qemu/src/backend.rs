use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendHandle, BackendStatus, DiskConfig, DiskFormat, HypervisorBackend,
    InstanceConfig, InstanceKind, InstanceState, LogLine, LogStreamSource, NatBackend,
    NetworkConfig, NetworkMode, RenderBackend, Resolution, ResourceMetrics,
};
use andler_net::{DefaultNetworkService, NetworkService};
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use crate::cmdline;
use crate::process::{ProcessError, QemuProcess};
use crate::qmp::{QmpClient, QmpError, VmStatus};

fn qmp_socket_dir() -> PathBuf {
    andler_core::paths::runtime_dir().join("andler/qmp")
}

struct RunningInstance {
    process: QemuProcess,
    qmp_client: Option<QmpClient>,
    /// QMP event reader task: connects to the dedicated events monitor
    /// socket and forwards async events to the backend's broadcast. The
    /// task reconnects while the instance lives; aborted on teardown.
    event_task: Option<tokio::task::JoinHandle<()>>,
    /// Name of the block graph's head node (the node whose file is the
    /// instance's `disk.qcow2`). After each live snapshot this becomes the
    /// new overlay's node; the next snapshot must target it, because
    /// `drive-disk0` is busy as the overlay's backing.
    head_node_name: String,

    network_info: NetworkInfo,
    /// (netdev id, host-side info) for every hotplugged extra NIC, so `stop` a…
    /// `detach_network` can tear down their taps/veths like the primary NIC's.
    extra_network_infos: Vec<(String, NetworkInfo)>,
}

#[derive(Clone)]
enum NetworkInfo {
    Bridge { bridge: String, tap_iface: String },
    Isolated { host_veth: String, vm_veth: String },
    Nat,
}

pub struct QemuBackend {
    instances: Mutex<HashMap<BackendHandle, RunningInstance>>,
    network_service: Arc<DefaultNetworkService>,
    qmp_events: tokio::sync::broadcast::Sender<andler_core::QmpEventRecord>,
    qmp_reconnects: std::sync::atomic::AtomicU64,
}

impl QemuBackend {
    pub fn new() -> Self {
        let (qmp_events, _) = tokio::sync::broadcast::channel(256);
        QemuBackend {
            instances: Mutex::new(HashMap::new()),
            network_service: Arc::new(DefaultNetworkService::new()),
            qmp_events,
            qmp_reconnects: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Maps a raw QMP event name onto the domain enum.
    fn map_qmp_event(name: &str) -> andler_core::QmpEvent {
        match name {
            "VSERPORT_CHANGED" => andler_core::QmpEvent::VserportChanged,
            "SHUTDOWN" => andler_core::QmpEvent::Shutdown,
            "RESET" => andler_core::QmpEvent::Reset,
            "POWERDOWN" => andler_core::QmpEvent::Powerdown,
            "DEVICE_DELETED" => andler_core::QmpEvent::DeviceDeleted,
            "BLOCK_IO_ERROR" => andler_core::QmpEvent::BlockIoError,
            "GUEST_PANICKED" => andler_core::QmpEvent::GuestPanicked,
            "WATCHDOG" => andler_core::QmpEvent::Watchdog,
            "STOP" => andler_core::QmpEvent::Stopped,
            "RESUME" => andler_core::QmpEvent::Resumed,
            name if name.starts_with("BLOCK_JOB") => andler_core::QmpEvent::BlockJob,
            _ => andler_core::QmpEvent::Other,
        }
    }

    fn handle_for(cfg: &InstanceConfig) -> BackendHandle {
        BackendHandle(format!("qemu:{}", cfg.id))
    }

    fn qmp_socket_path_for(cfg: &InstanceConfig) -> PathBuf {
        qmp_socket_dir().join(format!("{}.sock", cfg.id))
    }

    /// Spawns the events-monitor reader for a running instance. The task
    /// connects to `<qmp>.events.sock` — a dedicated second `-qmp` monitor
    /// that carries only async events, so event delivery can never
    /// interleave with command/reply traffic on the command monitor — and
    /// forwards every event to the backend broadcast. It reconnects with a
    /// short delay while the instance lives and is aborted on teardown.
    fn spawn_event_reader(&self, instance: &mut RunningInstance) {
        let events_socket =
            crate::cmdline::events_socket_path_for(instance.process.qmp_socket_path());
        let handle = BackendHandle(format!(
            "qemu:{}",
            instance
                .process
                .qmp_socket_path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
        ));
        let tx = self.qmp_events.clone();
        let task = tokio::spawn(async move {
            // §9.1.4: retry loops log coalesced — first failure, every
            // 50th, and the recovery summary — a dead socket must not
            // spam the daemon log nor vanish silently.
            let mut failed: u64 = 0;
            loop {
                match crate::qmp::QmpEventReader::connect(&events_socket).await {
                    Ok(reader) => {
                        if failed > 0 {
                            tracing::warn!(
                                handle = %handle.0,
                                attempts = failed,
                                "events monitor reconnected after {failed} failed attempts"
                            );
                            failed = 0;
                        }
                        let mut rx = reader.subscribe();
                        while let Ok(ev) = rx.recv().await {
                            let record = andler_core::QmpEventRecord {
                                handle: handle.clone(),
                                event: QemuBackend::map_qmp_event(&ev.event),
                                data: ev.data.map(|d| d.to_string()).unwrap_or_default(),
                            };
                            if tx.send(record).is_err() {
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        failed += 1;
                        if failed == 1 || failed.is_multiple_of(50) {
                            tracing::warn!(
                                handle = %handle.0,
                                attempts = failed,
                                error = %err,
                                "events monitor connect failed, retrying"
                            );
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
        instance.event_task = Some(task);
    }

    async fn ensure_qmp_connected(instance: &mut RunningInstance) -> Result<(), QmpError> {
        if instance.qmp_client.is_some() {
            return Ok(());
        }

        let path = instance.process.qmp_socket_path().clone();
        let mut last_connect_error = None;
        for _ in 0..300 {
            match QmpClient::connect(&path).await {
                Ok(client) => {
                    instance.qmp_client = Some(client);
                    return Ok(());
                }
                Err(err) => {
                    let retryable = match &err {
                        QmpError::ConnectFailed { source, .. } => matches!(
                            source.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ),
                        _ => false,
                    };
                    if retryable {
                        if !instance.process.is_alive() {
                            return Err(err);
                        }
                        last_connect_error = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    } else {
                        return Err(err);
                    }
                }
            }
        }
        // The loop only exits through a successful connect or a non-retryable
        // error, so this runs exactly after 300 retries (~30s).
        Err(last_connect_error.expect("retry loop always records its last error"))
    }

    /// Full guest argv for a package operation: the manager binary first, then
    /// the subcommand shape from `core::package_manager`. The args are
    /// subcommand-only by contract, so the binary name must be prepended here
    /// — regression: the online path once exec'd GNU coreutils' `install`.
    fn package_argv(
        pm: andler_core::package_manager::PackageManager,
        install: bool,
        package: &str,
    ) -> Vec<String> {
        let mut argv = vec![pm.binary_name().to_string()];
        let args = if install {
            pm.install_args(package)
        } else {
            pm.remove_args(package)
        };
        argv.extend(args.iter().map(|s| s.to_string()));
        argv
    }

    async fn guest_agent_client(instance: &mut RunningInstance) -> Result<QmpClient, BackendError> {
        let path = instance.process.qga_socket_path();
        QmpClient::connect_agent(&path)
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn diagnose_and_reset_qmp(
        instance: &mut RunningInstance,
        reconnects: &std::sync::atomic::AtomicU64,
    ) -> Option<BackendError> {
        reconnects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        instance.qmp_client = None;
        let alive = instance.process.is_alive();
        if alive {
            None
        } else {
            Some(BackendError::ProcessNotRunning)
        }
    }

    async fn guest_exec_package(
        &self,
        handle: &BackendHandle,
        package: &str,
        install: bool,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let mut qga = Self::guest_agent_client(instance).await?;

        const DETECT_SCRIPT: &str = "\
command -v apt-get >/dev/null 2>&1 && exit 0
command -v dnf >/dev/null 2>&1 && exit 10
command -v pacman >/dev/null 2>&1 && exit 20
exit 30";
        let pid = qga
            .guest_exec("/bin/sh", &["-c", DETECT_SCRIPT])
            .await
            .map_err(qmp_error_to_backend_error)?;

        let pkg_bin = wait_for_detection_exit(&mut qga, pid, std::time::Duration::from_secs(10))
            .await
            .map_err(qmp_error_to_backend_error)?;

        // The command shapes come from andler-core (the same single source
        // the offline chroot path uses) — the online QGA path must never
        // drift from the offline one.
        let pm = match pkg_bin {
            "apt-get" => andler_core::package_manager::PackageManager::Apt,
            "dnf" => andler_core::package_manager::PackageManager::Dnf,
            "pacman" => andler_core::package_manager::PackageManager::Pacman,
            _ => andler_core::package_manager::PackageManager::Apt,
        };
        // install_args/remove_args carry the subcommand only ("install -y
        // <pkg>"); the manager binary itself must head the argv — without
        // it the guest runs GNU coreutils' `install` instead of apt-get.
        let argv = Self::package_argv(pm, install, package);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

        let pid = qga
            .guest_exec(argv_refs[0], &argv_refs[1..])
            .await
            .map_err(qmp_error_to_backend_error)?;

        let result = wait_for_guest_exec(&mut qga, pid, std::time::Duration::from_secs(60))
            .await
            .map_err(qmp_error_to_backend_error)?;

        if let Some(_output) = result {
            // Guest-exec stdout/stderr is never logged (§9.1.5): it can
            // carry passwords or tokens the guest printed; failures surface
            // through the returned error instead.
            tracing::info!(
                package = %package,
                install = %install,
                "guest-exec completed"
            );
        }

        Ok(())
    }

    fn instance_id_from_handle(handle: &BackendHandle) -> Result<&str, BackendError> {
        handle
            .0
            .strip_prefix("qemu:")
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))
    }

    async fn setup_extra_network(
        &self,
        network: &NetworkConfig,
        index: usize,
        instance_id: &str,
    ) -> Result<NetworkInfo, BackendError> {
        match &network.mode {
            NetworkMode::Bridge { interface: bridge } => {
                let tap_iface = cmdline::extra_net_bridge_tap_iface(instance_id, index);
                self.network_service
                    .setup_bridge(bridge, &tap_iface)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
                Ok(NetworkInfo::Bridge {
                    bridge: bridge.clone(),
                    tap_iface,
                })
            }
            NetworkMode::Isolated => {
                let vm_iface = cmdline::extra_net_isolated_iface(index);
                let (host_veth, vm_veth) = self
                    .network_service
                    .setup_isolated(&vm_iface)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
                Ok(NetworkInfo::Isolated { host_veth, vm_veth })
            }
            NetworkMode::Nat => Ok(NetworkInfo::Nat),
        }
    }

    async fn teardown_network_info(&self, info: &NetworkInfo) {
        match info {
            NetworkInfo::Bridge { bridge, tap_iface } => {
                let _ = self
                    .network_service
                    .teardown_bridge(bridge, tap_iface)
                    .await;
            }
            NetworkInfo::Isolated { host_veth, vm_veth } => {
                let _ = self
                    .network_service
                    .teardown_isolated(host_veth, vm_veth)
                    .await;
            }
            NetworkInfo::Nat => {}
        }
    }

    /// Runs a QMP operation with the same recovery contract as `pause`/`resume`:
    /// connect if needed; fail immediately on a command-level error (QEMU already
    /// answered, retrying changes nothing); on a connection-level error, clear the
    /// stale client, verify the process is alive, reconnect, and run the op once
    /// more. `op` must be callable twice (recoverable connection errors), so
    /// closures should clone their captured values per call.
    async fn run_qmp_operation<T, F>(
        instance: &mut RunningInstance,
        reconnects: &std::sync::atomic::AtomicU64,
        mut op: F,
    ) -> Result<T, BackendError>
    where
        F: for<'a> FnMut(
            &'a mut QmpClient,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, QmpError>> + Send + 'a>,
        >,
    {
        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let first_attempt = op(instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded"))
        .await;

        match first_attempt {
            Ok(value) => Ok(value),
            Err(err)
                if matches!(
                    err,
                    QmpError::CommandFailed { .. } | QmpError::ParseError(_)
                ) =>
            {
                Err(qmp_error_to_backend_error(err))
            }
            Err(_connection_level_err) => {
                if let Some(err) = Self::diagnose_and_reset_qmp(instance, reconnects).await {
                    return Err(err);
                }
                Self::ensure_qmp_connected(instance)
                    .await
                    .map_err(qmp_error_to_backend_error)?;
                op(instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded"))
                .await
                .map_err(qmp_error_to_backend_error)
            }
        }
    }
}

impl Default for QemuBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn process_error_to_backend_error(err: ProcessError) -> BackendError {
    BackendError::Io(err.to_string())
}

fn qmp_error_to_backend_error(err: QmpError) -> BackendError {
    BackendError::Io(err.to_string())
}

async fn wait_for_guest_exec(
    qmp: &mut QmpClient,
    pid: u64,
    timeout: std::time::Duration,
) -> Result<Option<String>, QmpError> {
    use std::time::Instant;
    let start = Instant::now();

    loop {
        let status = qmp.guest_exec_status(pid).await?;

        if status.exited {
            if status.exitcode.unwrap_or_default() != 0 {
                let stderr =
                    crate::qmp::decode_guest_exec_data(status.err_data).unwrap_or_default();
                return Err(QmpError::CommandFailed {
                    command: format!("guest-exec pid={pid}"),
                    class: "GuestExecFailed".to_string(),
                    desc: format!(
                        "guest process exited with code {}: {}",
                        status.exitcode.unwrap_or_default(),
                        stderr.trim()
                    ),
                });
            }
            return Ok(crate::qmp::decode_guest_exec_data(status.out_data));
        }

        if start.elapsed() > timeout {
            return Err(QmpError::CommandFailed {
                command: format!("guest-exec pid={pid}"),
                class: "Timeout".to_string(),
                desc: format!("guest process did not exit within {:?}", timeout),
            });
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn wait_for_detection_exit(
    qmp: &mut QmpClient,
    pid: u64,
    timeout: std::time::Duration,
) -> Result<&'static str, QmpError> {
    use std::time::Instant;
    let start = Instant::now();
    loop {
        let status = qmp.guest_exec_status(pid).await?;
        if status.exited {
            return match status.exitcode.unwrap_or_default() {
                0 => Ok("apt-get"),
                10 => Ok("dnf"),
                20 => Ok("pacman"),
                code => Err(QmpError::CommandFailed {
                    command: format!("guest-exec pid={pid}"),
                    class: "GuestExecFailed".to_string(),
                    desc: format!("no supported package manager found in guest (exit {code})"),
                }),
            };
        }
        if start.elapsed() > timeout {
            return Err(QmpError::CommandFailed {
                command: format!("guest-exec pid={pid}"),
                class: "Timeout".to_string(),
                desc: format!(
                    "package manager detection did not finish within {:?}",
                    timeout
                ),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

const DISK_DEVICE: &str = "drive-disk0";

/// QMP node name for an overlay file. The daemon names live-snapshot
/// overlays `.tmp-<uuid>.qcow2`; the uuid keeps node names unique per
/// instance session. QEMU caps node names at 31 chars, so the uuid is
/// truncated to 24 hex digits (96 bits of uniqueness).
fn snapshot_node_name(layer_path: &std::path::Path) -> String {
    let stem = layer_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("overlay");
    let stem = stem.strip_prefix(".tmp-").unwrap_or(stem);
    let stem: String = stem.chars().take(24).collect();
    format!("snap-{stem}")
}

async fn read_log_history(path: &std::path::Path) -> Vec<LogLine> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| {
            if let Some(rest) = line.strip_prefix("[stdout] ") {
                Some(LogLine {
                    source: LogStreamSource::Stdout,
                    line: rest.to_string(),
                })
            } else {
                line.strip_prefix("[stderr] ").map(|rest| LogLine {
                    source: LogStreamSource::Stderr,
                    line: rest.to_string(),
                })
            }
        })
        .collect()
}

fn vm_status_to_instance_state(status: VmStatus) -> Option<InstanceState> {
    match status {
        VmStatus::Running => Some(InstanceState::Running),
        VmStatus::Paused => Some(InstanceState::Paused),
        VmStatus::Shutdown => Some(InstanceState::Stopped),
        VmStatus::Other => None,
    }
}

/// Guard against pid reuse when adopting: the QMP socket pins the identity of
/// the QEMU process, but the pid we get from it must still match the cmdline
/// marker `process=<name>` that cmdline.rs puts into `-name` for this
/// instance — otherwise the daemon would pidfd-wait on an unrelated process.
fn verify_cmdline_marker(pid: u32, instance_name: &str) -> Result<(), String> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map_err(|e| format!("cannot read /proc/{pid}/cmdline: {e}"))?;
    let marker = format!("process={instance_name}");
    for arg in cmdline.split(|b| *b == 0) {
        if arg.windows(marker.len()).any(|w| w == marker.as_bytes()) {
            return Ok(());
        }
    }
    Err(format!(
        "/proc/{pid}/cmdline does not contain the `{marker}` marker"
    ))
}

impl QemuBackend {
    /// Resolves the host pid of the QEMU process holding `qmp_socket_path`.
    /// `query-processes` is the cheap direct answer but is not present in
    /// every QEMU build (CommandNotFound). The fallback scans /proc for the
    /// `process=` cmdline marker (the marker exists precisely so an orphaned
    /// QEMU can be recognized after a daemon restart). Reading /proc/<pid>/fd of an
    /// orphaned process is blocked by Yama ptrace_scope, so socket-inode
    /// matching cannot be the primary path — the marker scan is, and the
    /// already-succeeded QMP connection plus the instance-owned socket
    /// location pin the identity; the marker guards against pid reuse.
    async fn resolve_owner_pid(
        qmp: &mut QmpClient,
        qmp_socket_path: &std::path::Path,
        instance_name: &str,
    ) -> Result<u32, String> {
        if let Ok(pid) = qmp.query_process_pid().await {
            return verify_cmdline_marker(pid, instance_name)
                .map(|()| pid)
                .map_err(|reason| {
                    format!(
                        "QMP gives owner pid {pid} but it is not the expected instance \
                         process: {reason}"
                    )
                });
        }

        let mut candidates = Vec::new();
        let proc = std::fs::read_dir("/proc").map_err(|e| format!("cannot scan /proc: {e}"))?;
        for entry in proc.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if verify_cmdline_marker(pid, instance_name).is_ok() {
                candidates.push(pid);
            }
        }
        match candidates.as_slice() {
            [] => Err(format!(
                "no live process carries the `process={instance_name}` marker — \
                 the instance QEMU either died or never ran"
            )),
            [single] => Ok(*single),
            many => {
                // Multiple instances may share a name; the one that owns the
                // QMP socket is the one we connected to. Its listen socket's
                // inode equals the socket file's inode.
                let Ok(meta) = std::fs::metadata(qmp_socket_path) else {
                    return Err(format!("cannot stat {}", qmp_socket_path.display()));
                };
                let socket_ino = std::os::unix::fs::MetadataExt::ino(&meta);
                for pid in many {
                    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                        continue;
                    };
                    let holds_socket = fds.flatten().any(|fd| {
                        std::fs::read_link(fd.path())
                            .ok()
                            .map(|target| target.to_string_lossy().into_owned())
                            .is_some_and(|target| target == format!("socket:[{socket_ino}]"))
                    });
                    if holds_socket {
                        return Ok(*pid);
                    }
                }
                Err(format!(
                    "multiple processes carry the `process={instance_name}` marker \
                     ({many:?}) and none can be tied to {}",
                    qmp_socket_path.display()
                ))
            }
        }
    }
}

#[async_trait]
impl HypervisorBackend for QemuBackend {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn supported_render_backends(&self) -> &[RenderBackend] {
        &[
            RenderBackend::Venus,
            RenderBackend::VirtioGpu,
            RenderBackend::VirGl,
            RenderBackend::Cpu,
        ]
    }

    fn subscribe_qmp_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<andler_core::QmpEventRecord>> {
        Some(self.qmp_events.subscribe())
    }

    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        if !cfg.gpu.render_backend.is_implemented() {
            return Err(BackendError::InvalidConfig {
                backend: "qemu",
                reason: format!(
                    "render backend {:?} is not implemented",
                    cfg.gpu.render_backend
                ),
            });
        }

        let handle = Self::handle_for(cfg);
        let qmp_socket_path = Self::qmp_socket_path_for(cfg);

        if let Some(parent) = qmp_socket_path.parent() {
            andler_core::paths::ensure_private_dir(parent)
                .await
                .map_err(|e| BackendError::Io(e.to_string()))?;
        }

        // QEMU does not create parent directories for its own sockets; the
        // serial console chardev needs its directory provisioned up front.
        if let Some(parent) = andler_core::paths::console_socket_path(&cfg.id).parent() {
            andler_core::paths::ensure_private_dir(parent)
                .await
                .map_err(|e| BackendError::Io(e.to_string()))?;
        }

        let _ = tokio::fs::remove_file(&qmp_socket_path).await;

        let args = cmdline::build_args(cfg, &qmp_socket_path)?;

        let log_file_path = cfg.disk.path.parent().map(|dir| dir.join("qemu.log"));

        let network_info = match &cfg.network.mode {
            NetworkMode::Bridge { interface: bridge } => {
                let tap_iface = cmdline::primary_net_bridge_tap_iface(&cfg.id.to_string());
                self.network_service
                    .setup_bridge(bridge, &tap_iface)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
                NetworkInfo::Bridge {
                    bridge: bridge.clone(),
                    tap_iface,
                }
            }
            NetworkMode::Isolated => {
                let vm_iface = "andler0";
                let (host_veth, vm_veth) = self
                    .network_service
                    .setup_isolated(vm_iface)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
                NetworkInfo::Isolated { host_veth, vm_veth }
            }
            NetworkMode::Nat => NetworkInfo::Nat,
        };
        let mut extra_network_infos = Vec::new();
        for (index, extra) in cfg.extra_networks.iter().enumerate() {
            match self
                .setup_extra_network(extra, index, &cfg.id.to_string())
                .await
            {
                Ok(info) => extra_network_infos.push((cmdline::extra_net_id(index), info)),
                Err(err) => {
                    self.teardown_network_info(&network_info).await;
                    for (_, info) in &extra_network_infos {
                        self.teardown_network_info(info).await;
                    }
                    return Err(err);
                }
            }
        }

        // ANDLERD_DEV_RESTART=1 keeps the QEMU process alive when the daemon
        // exits, so a dev loop can restart the daemon without losing VMs.
        let dev_restart = std::env::var("ANDLERD_DEV_RESTART")
            .map(|value| value == "1")
            .unwrap_or(false);
        let process = QemuProcess::spawn(&args, qmp_socket_path, log_file_path, !dev_restart).await;
        if let Err(err) = process {
            self.teardown_network_info(&network_info).await;
            for (_, info) in &extra_network_infos {
                self.teardown_network_info(info).await;
            }
            return Err(process_error_to_backend_error(err));
        }
        let process = process.unwrap();

        let mut instances = self.instances.lock().await;
        instances.insert(
            handle.clone(),
            RunningInstance {
                process,
                qmp_client: None,
                event_task: None,
                head_node_name: DISK_DEVICE.to_string(),
                network_info,
                extra_network_infos,
            },
        );
        if let Some(instance) = instances.get_mut(&handle) {
            self.spawn_event_reader(instance);
        }

        Ok(handle)
    }

    async fn adopt(
        &self,
        cfg: &InstanceConfig,
    ) -> Result<(BackendHandle, InstanceState), BackendError> {
        let handle = Self::handle_for(cfg);
        let qmp_socket_path = Self::qmp_socket_path_for(cfg);

        let mut qmp = QmpClient::connect(&qmp_socket_path)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let vm_status = qmp
            .query_status()
            .await
            .map_err(qmp_error_to_backend_error)?;
        let state = vm_status_to_instance_state(vm_status)
            .filter(|s| matches!(s, InstanceState::Running | InstanceState::Paused))
            .ok_or_else(|| {
                BackendError::Io(format!(
                    "QMP socket {path} answers, but the VM is {status:?} — not a state a \
                     restarted daemon can adopt",
                    path = qmp_socket_path.display(),
                    status = vm_status
                ))
            })?;

        let pid = Self::resolve_owner_pid(&mut qmp, &qmp_socket_path, &cfg.name)
            .await
            .map_err(BackendError::Io)?;

        let log_file_path = cfg.disk.path.parent().map(|dir| dir.join("qemu.log"));
        let process = QemuProcess::adopt(pid, qmp_socket_path, log_file_path)
            .map_err(process_error_to_backend_error)?;

        // Host-side network state is deterministic for Bridge (the tap name
        // derives from the instance id), so `stop` can still tear it down;
        // Isolated mode cannot be alive (setup always fails today) and Nat
        // needs no teardown, so both degrade to NetworkInfo::Nat.
        let network_info = match &cfg.network.mode {
            NetworkMode::Bridge { interface } => NetworkInfo::Bridge {
                bridge: interface.clone(),
                tap_iface: cmdline::primary_net_bridge_tap_iface(&cfg.id.to_string()),
            },
            NetworkMode::Isolated | NetworkMode::Nat => NetworkInfo::Nat,
        };

        // The adopted QEMU may have live snapshot overlays from before the
        // daemon died: the head node is the one whose file is the active
        // disk (snapshots keep the file open under their staging name).
        let active_disk = cfg.disk.path.to_string_lossy().into_owned();
        let head_node_name = qmp
            .query_head_node_name(&active_disk)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let mut instances = self.instances.lock().await;
        instances.insert(
            handle.clone(),
            RunningInstance {
                process,
                qmp_client: Some(qmp),
                event_task: None,
                head_node_name,
                network_info,
                extra_network_infos: Vec::new(),
            },
        );
        if let Some(instance) = instances.get_mut(&handle) {
            self.spawn_event_reader(instance);
        }

        Ok((handle, state))
    }

    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let first_attempt = instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .pause()
            .await;

        match first_attempt {
            Ok(()) => Ok(()),
            Err(err)
                if matches!(
                    err,
                    QmpError::CommandFailed { .. } | QmpError::ParseError(_)
                ) =>
            {
                Err(qmp_error_to_backend_error(err))
            }
            Err(_connection_level_err) => {
                if let Some(err) =
                    Self::diagnose_and_reset_qmp(instance, &self.qmp_reconnects).await
                {
                    return Err(err);
                }
                Self::ensure_qmp_connected(instance)
                    .await
                    .map_err(qmp_error_to_backend_error)?;
                instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded")
                    .pause()
                    .await
                    .map_err(qmp_error_to_backend_error)
            }
        }
    }

    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let first_attempt = instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .resume()
            .await;

        match first_attempt {
            Ok(()) => Ok(()),
            Err(err)
                if matches!(
                    err,
                    QmpError::CommandFailed { .. } | QmpError::ParseError(_)
                ) =>
            {
                Err(qmp_error_to_backend_error(err))
            }
            Err(_connection_level_err) => {
                if let Some(err) =
                    Self::diagnose_and_reset_qmp(instance, &self.qmp_reconnects).await
                {
                    return Err(err);
                }
                Self::ensure_qmp_connected(instance)
                    .await
                    .map_err(qmp_error_to_backend_error)?;
                instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded")
                    .resume()
                    .await
                    .map_err(qmp_error_to_backend_error)
            }
        }
    }

    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        if graceful {
            instance
                .process
                .terminate()
                .await
                .map_err(process_error_to_backend_error)?;
        } else {
            instance
                .process
                .force_kill()
                .await
                .map_err(process_error_to_backend_error)?;
        }

        let network_info = instance.network_info.clone();
        let extra_network_infos = std::mem::take(&mut instance.extra_network_infos);
        if let Some(task) = instance.event_task.take() {
            task.abort();
        }
        instances.remove(handle);

        match network_info {
            NetworkInfo::Bridge { bridge, tap_iface } => {
                self.network_service
                    .teardown_bridge(&bridge, &tap_iface)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
            }
            NetworkInfo::Isolated { host_veth, vm_veth } => {
                self.network_service
                    .teardown_isolated(&host_veth, &vm_veth)
                    .await
                    .map_err(|e| BackendError::Io(e.to_string()))?;
            }
            NetworkInfo::Nat => {}
        }

        for (_, info) in &extra_network_infos {
            match info {
                NetworkInfo::Bridge { bridge, tap_iface } => {
                    self.network_service
                        .teardown_bridge(bridge, tap_iface)
                        .await
                        .map_err(|e| BackendError::Io(e.to_string()))?;
                }
                NetworkInfo::Isolated { host_veth, vm_veth } => {
                    self.network_service
                        .teardown_isolated(host_veth, vm_veth)
                        .await
                        .map_err(|e| BackendError::Io(e.to_string()))?;
                }
                NetworkInfo::Nat => {}
            }
        }

        Ok(())
    }

    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let alive = instance.process.is_alive();

        if !alive {
            return Ok(BackendStatus {
                state: InstanceState::Stopped,
                detail: Some("process is not running".to_string()),
                clean_shutdown: false,
            });
        }

        match Self::ensure_qmp_connected(instance).await {
            Ok(()) => {
                let qmp_status = instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded")
                    .query_status()
                    .await;

                match qmp_status {
                    Ok(vm_status) => match vm_status_to_instance_state(vm_status) {
                        Some(InstanceState::Stopped) => Ok(BackendStatus {
                            state: InstanceState::Stopped,
                            detail: Some(
                                "guest shut down cleanly (QMP reports VM status Shutdown)"
                                    .to_string(),
                            ),
                            clean_shutdown: true,
                        }),
                        Some(state) => Ok(BackendStatus {
                            state,
                            detail: None,
                            clean_shutdown: false,
                        }),
                        None => Ok(BackendStatus {
                            state: InstanceState::Running,
                            detail: Some(format!(
                                "process is alive; QMP reports VM status {vm_status:?}, \
                                 which does not map directly to a HypervisorBackend state"
                            )),
                            clean_shutdown: false,
                        }),
                    },
                    Err(qmp_err) => {
                        instance.qmp_client = None;
                        Ok(BackendStatus {
                            state: InstanceState::Running,
                            detail: Some(format!(
                                "process is alive but QMP query-status failed: {qmp_err}"
                            )),
                            clean_shutdown: false,
                        })
                    }
                }
            }
            Err(qmp_err) => Ok(BackendStatus {
                state: InstanceState::Running,
                detail: Some(format!(
                    "process is alive but QMP connection failed: {qmp_err}"
                )),
                clean_shutdown: false,
            }),
        }
    }

    async fn snapshot(
        &self,
        handle: &BackendHandle,
        layer_path: &std::path::Path,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let node_name = snapshot_node_name(layer_path);
        let path = layer_path.to_string_lossy().into_owned();
        let head = instance.head_node_name.clone();
        let qmp = instance.qmp_client.as_mut().expect("just connected");
        qmp.blockdev_add_overlay(&node_name, &path)
            .await
            .map_err(qmp_error_to_backend_error)?;
        qmp.blockdev_snapshot(&head, &node_name)
            .await
            .map_err(qmp_error_to_backend_error)?;
        instance.head_node_name = node_name;

        Ok(())
    }

    async fn snapshot_restore(
        &self,
        _handle: &BackendHandle,
        _tag: &str,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotImplemented {
            backend: "qemu",
            operation: "snapshot_restore (restore is an offline qemu-img operation, see daemon restore_snapshot)",
        })
    }

    async fn snapshot_delete(
        &self,
        handle: &BackendHandle,
        tag: &str,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        qmp.snapshot_delete(DISK_DEVICE, tag)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(())
    }

    async fn snapshot_list(
        &self,
        handle: &BackendHandle,
    ) -> Result<Vec<andler_core::SnapshotInfo>, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        let snapshots = qmp
            .query_block_snapshots(DISK_DEVICE)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(snapshots
            .into_iter()
            .map(|s| andler_core::SnapshotInfo {
                tag: s.tag,
                id: s.id,
                created_at: s.datetime,
            })
            .collect())
    }

    async fn attach_disk(
        &self,
        handle: &BackendHandle,
        disk: &DiskConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let drive_id = cmdline::extra_disk_drive_id(index);
        let device_id = cmdline::extra_disk_device_id(index);
        let path = disk.path.to_string_lossy().into_owned();
        let format_str = match disk.format {
            DiskFormat::Qcow2 => "qcow2",
            DiskFormat::Raw => "raw",
            DiskFormat::Vdi => "vdi",
        };

        Self::run_qmp_operation(instance, &self.qmp_reconnects, move |qmp| {
            let drive_id = drive_id.clone();
            let device_id = device_id.clone();
            let path = path.clone();
            Box::pin(async move {
                qmp.blockdev_add(&drive_id, &path, format_str).await?;
                match qmp.device_add_block(&device_id, &drive_id, index).await {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        // Roll back the block node so a failed device_add leaves no
                        // orphan: a detach later could never reach it anyway.
                        let _ = qmp.blockdev_del(&drive_id).await;
                        Err(err)
                    }
                }
            })
        })
        .await
    }

    async fn detach_disk(&self, handle: &BackendHandle, index: usize) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let drive_id = cmdline::extra_disk_drive_id(index);
        let device_id = cmdline::extra_disk_device_id(index);

        Self::run_qmp_operation(instance, &self.qmp_reconnects, move |qmp| {
            let drive_id = drive_id.clone();
            let device_id = device_id.clone();
            Box::pin(async move {
                qmp.detach_block_device(&device_id, &drive_id, std::time::Duration::from_secs(15))
                    .await
            })
        })
        .await
    }

    async fn attach_network(
        &self,
        handle: &BackendHandle,
        network: &NetworkConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;
        let instance_id = Self::instance_id_from_handle(handle)?.to_string();

        // Host-side tap/veth first, so a failure leaves the VM completely untouched.
        let info = self
            .setup_extra_network(network, index, &instance_id)
            .await?;

        let network = network.clone();
        let model = network.device_model.clone();
        let result = Self::run_qmp_operation(instance, &self.qmp_reconnects, move |qmp| {
            let model = model.clone();
            let network = network.clone();
            let instance_id = instance_id.clone();
            Box::pin(async move {
                let netdev_id = cmdline::extra_net_id(index);
                match &network.mode {
                    NetworkMode::Nat => match network.nat_backend {
                        NatBackend::Slirp => qmp.netdev_add_user(&netdev_id).await?,
                        NatBackend::Passt => qmp.netdev_add_passt(&netdev_id).await?,
                    },
                    NetworkMode::Bridge { .. } => {
                        let ifname = cmdline::extra_net_bridge_tap_iface(&instance_id, index);
                        qmp.netdev_add_tap(&netdev_id, &ifname).await?
                    }
                    NetworkMode::Isolated => {
                        let ifname = cmdline::extra_net_isolated_iface(index);
                        qmp.netdev_add_tap(&netdev_id, &ifname).await?
                    }
                }
                qmp.device_add_net(&netdev_id, &netdev_id, &model, index)
                    .await
            })
        })
        .await;

        match result {
            Ok(()) => {
                let netdev_id = cmdline::extra_net_id(index);
                instance.extra_network_infos.push((netdev_id, info));
                Ok(())
            }
            Err(err) => {
                self.teardown_network_info(&info).await;
                Err(err)
            }
        }
    }

    async fn detach_network(
        &self,
        handle: &BackendHandle,
        index: usize,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::run_qmp_operation(instance, &self.qmp_reconnects, move |qmp| {
            Box::pin(async move {
                let netdev_id = cmdline::extra_net_id(index);
                qmp.detach_net_device(&netdev_id, &netdev_id, std::time::Duration::from_secs(15))
                    .await
            })
        })
        .await?;

        let netdev_id = cmdline::extra_net_id(index);
        if let Some(pos) = instance
            .extra_network_infos
            .iter()
            .position(|(id, _)| *id == netdev_id)
        {
            let (_, info) = instance.extra_network_infos.remove(pos);
            self.teardown_network_info(&info).await;
        }

        Ok(())
    }

    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics> {
        let handle = handle.clone();
        let stream = futures_util::stream::once(async move {
            let mut instances = self.instances.lock().await;
            match instances.get_mut(&handle) {
                Some(i) => {
                    let receiver = i.process.subscribe_metrics();
                    Box::pin(
                        tokio_stream::wrappers::BroadcastStream::new(receiver)
                            .filter_map(|item| async move { item.ok() }),
                    ) as BoxStream<'_, ResourceMetrics>
                }
                None => Box::pin(futures_util::stream::empty()) as BoxStream<'_, ResourceMetrics>,
            }
        })
        .flatten();

        Box::pin(stream)
    }

    fn process_exit_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ()> {
        let handle = handle.clone();
        let stream = futures_util::stream::once(async move {
            let mut instances = self.instances.lock().await;
            match instances.get_mut(&handle) {
                Some(i) => {
                    let receiver = i.process.subscribe_exit();
                    Box::pin(
                        tokio_stream::wrappers::BroadcastStream::new(receiver)
                            .filter_map(|item| async move { item.ok() }),
                    ) as BoxStream<'_, ()>
                }
                None => Box::pin(futures_util::stream::empty()) as BoxStream<'_, ()>,
            }
        })
        .flatten();

        Box::pin(stream)
    }

    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine> {
        let handle = handle.clone();
        let stream = futures_util::stream::once(async move {
            let mut instances = self.instances.lock().await;
            let subscription = instances.get_mut(&handle).map(|i| {
                (
                    i.process.log_file_path().map(PathBuf::from),
                    i.process.subscribe_logs(),
                    i.process.subscribe_exit(),
                    !i.process.is_alive(),
                )
            });

            match subscription {
                Some((log_file_path, receiver, exit_receiver, already_dead)) => {
                    let live = tokio_stream::wrappers::BroadcastStream::new(receiver)
                        .filter_map(|item| async move { item.ok() });

                    let history = futures_util::stream::once(async move {
                        match log_file_path {
                            Some(path) => read_log_history(&path).await,
                            None => Vec::new(),
                        }
                    })
                    .flat_map(futures_util::stream::iter);

                    // A dead process must end the log stream: the history is
                    // everything it will ever emit. Live receivers only
                    // observe the buffered exit value when the death happened
                    // after subscription, so an already-dead process skips
                    // the wait entirely.
                    let done = async move {
                        let mut exit_receiver = exit_receiver;
                        if !already_dead {
                            let _ = exit_receiver.recv().await;
                        }
                    };

                    Box::pin(history.chain(live.take_until(done))) as BoxStream<'_, LogLine>
                }
                None => Box::pin(futures_util::stream::empty()) as BoxStream<'_, LogLine>,
            }
        })
        .flatten();

        Box::pin(stream)
    }

    async fn is_guest_agent_available(&self, handle: &BackendHandle) -> Result<bool, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let mut qga = Self::guest_agent_client(instance).await?;
        Ok(qga.is_guest_agent_available().await)
    }

    async fn guest_exec_install(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, true).await
    }

    async fn guest_exec_remove(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, false).await
    }

    async fn guest_exec_command(
        &self,
        handle: &BackendHandle,
        argv: &[String],
        timeout: Option<std::time::Duration>,
    ) -> Result<andler_core::GuestExecOutput, BackendError> {
        if argv.is_empty() {
            return Err(BackendError::Io(
                "guest exec needs at least a program".to_string(),
            ));
        }
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let mut qga = Self::guest_agent_client(instance).await?;

        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let pid = qga
            .guest_exec(&argv[0], &args)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let timeout = timeout.unwrap_or(std::time::Duration::from_secs(60));
        use std::time::Instant;
        let start = Instant::now();
        loop {
            let status = qga
                .guest_exec_status(pid)
                .await
                .map_err(qmp_error_to_backend_error)?;
            if status.exited {
                return Ok(andler_core::GuestExecOutput {
                    exit_code: status.exitcode.unwrap_or(-1) as i32,
                    stdout: crate::qmp::decode_guest_exec_data(status.out_data).unwrap_or_default(),
                    stderr: crate::qmp::decode_guest_exec_data(status.err_data).unwrap_or_default(),
                });
            }
            if start.elapsed() > timeout {
                return Err(qmp_error_to_backend_error(QmpError::CommandFailed {
                    command: format!("guest-exec pid={pid}"),
                    class: "Timeout".to_string(),
                    desc: format!("guest command did not finish within {timeout:?}"),
                }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn guest_mutator(
        &self,
        handle: &BackendHandle,
    ) -> Result<Box<dyn andler_core::GuestMutator>, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;
        let qga = Self::guest_agent_client(instance).await?;
        Ok(Box::new(crate::mutator::QgaMutator::new(qga)))
    }

    fn qmp_reconnect_count(&self) -> u64 {
        self.qmp_reconnects
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn set_guest_display_resolution(
        &self,
        handle: &BackendHandle,
        resolution: &Resolution,
        kind: InstanceKind,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let mut qga = Self::guest_agent_client(instance).await?;

        let content = format!("RESOLUTION={}x{}\n", resolution.width, resolution.height);
        qga.guest_file_write("/etc/andler/display.conf", &content)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let (cmd, args): (&str, &[&str]) = match kind {
            InstanceKind::AndroidVm { .. } => {
                ("systemctl", &["restart", "waydroid-compositor.service"])
            }
            InstanceKind::LinuxVm { .. } => ("/usr/local/bin/andler-apply-resolution", &[]),
        };
        let pid = qga
            .guest_exec(cmd, args)
            .await
            .map_err(qmp_error_to_backend_error)?;
        let output = wait_for_guest_exec(&mut qga, pid, std::time::Duration::from_secs(30))
            .await
            .map_err(qmp_error_to_backend_error)?;
        if let Some(output) = output {
            tracing::debug!(
                resolution = %format!("{}x{}", resolution.width, resolution.height),
                output = %output,
                "guest display resolution applied"
            );
        }
        Ok(())
    }

    async fn guest_check_binary_installed(
        &self,
        handle: &BackendHandle,
        binary_path: &str,
    ) -> Result<bool, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let mut qga = Self::guest_agent_client(instance).await?;

        let pid = qga
            .guest_exec("/usr/bin/test", &["-x", binary_path])
            .await
            .map_err(qmp_error_to_backend_error)?;

        use std::time::Instant;
        let start = Instant::now();
        let timeout = std::time::Duration::from_secs(5);

        loop {
            let status = qga
                .guest_exec_status(pid)
                .await
                .map_err(qmp_error_to_backend_error)?;

            if status.exited {
                return Ok(status.exitcode.unwrap_or_default() == 0);
            }

            if start.elapsed() > timeout {
                return Ok(false);
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig,
        GpuConfig, InputConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config(render_backend: RenderBackend) -> InstanceConfig {
        let mut gpu = GpuConfig::reference_default();
        gpu.render_backend = render_backend;

        // The shared /tmp/test-disk.qcow2 is left as a 0-byte stub by other
        // crates' tests; QEMU's -blockdev validates the qcow2 header at
        // startup and dies on it, so the fixture must be a real image.
        ensure_test_disk();

        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            schema_version: andler_core::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu,
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
            autostart: false,
        }
    }

    #[test]
    fn name_returns_qemu() {
        let backend = QemuBackend::new();
        assert_eq!(backend.name(), "qemu");
    }

    #[test]
    fn supported_render_backends_excludes_passthrough() {
        let backend = QemuBackend::new();
        let supported = backend.supported_render_backends();
        assert!(supported.contains(&RenderBackend::Venus));
        assert!(!supported
            .iter()
            .any(|b| matches!(b, RenderBackend::Passthrough { .. })));
    }

    #[tokio::test]
    async fn spawn_rejects_passthrough_before_touching_process() {
        let backend = QemuBackend::new();
        let cfg = sample_config(RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        });

        let err = backend.spawn(&cfg).await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidConfig { .. }));
    }

    #[tokio::test]
    async fn stop_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.stop(&unknown, true).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn status_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.status(&unknown).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn pause_resume_on_unknown_handle_return_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        assert!(matches!(
            backend.pause(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
        assert!(matches!(
            backend.resume(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn metrics_stream_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.metrics_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn log_stream_on_unknown_handle_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.log_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "andler-qemu-test-{}-{}",
                std::process::id(),
                InstanceId::new().to_string()
            ));
            std::fs::create_dir_all(&path).expect("create test temp dir");
            TestTempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn read_log_history_parses_stdout_and_stderr_lines_in_order() {
        let dir = TestTempDir::new();
        let log_path = dir.path().join("qemu.log");
        tokio::fs::write(
            &log_path,
            "[stdout] QEMU 8.2.0 starting\n[stderr] warning: no display\n[stdout] guest booted\n",
        )
        .await
        .expect("write test qemu.log");

        let history = read_log_history(&log_path).await;

        assert_eq!(
            history,
            vec![
                LogLine {
                    source: LogStreamSource::Stdout,
                    line: "QEMU 8.2.0 starting".to_string(),
                },
                LogLine {
                    source: LogStreamSource::Stderr,
                    line: "warning: no display".to_string(),
                },
                LogLine {
                    source: LogStreamSource::Stdout,
                    line: "guest booted".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn read_log_history_returns_empty_for_missing_file() {
        let dir = TestTempDir::new();
        let missing_path = dir.path().join("never-created-qemu.log");
        assert_eq!(read_log_history(&missing_path).await, Vec::new());
    }

    #[tokio::test]
    async fn read_log_history_skips_lines_without_a_known_prefix() {
        let dir = TestTempDir::new();
        let log_path = dir.path().join("qemu.log");
        tokio::fs::write(&log_path, "garbage line\n[stdout] real line\n")
            .await
            .expect("write test qemu.log");

        let history = read_log_history(&log_path).await;

        assert_eq!(
            history,
            vec![LogLine {
                source: LogStreamSource::Stdout,
                line: "real line".to_string(),
            }]
        );
    }

    #[test]
    fn vm_status_maps_running_and_paused_directly() {
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Running),
            Some(InstanceState::Running)
        );
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Paused),
            Some(InstanceState::Paused)
        );
    }

    #[test]
    fn vm_status_maps_shutdown_to_stopped() {
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Shutdown),
            Some(InstanceState::Stopped)
        );
    }

    #[test]
    fn vm_status_other_does_not_map_directly() {
        assert_eq!(vm_status_to_instance_state(VmStatus::Other), None);
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    async fn spawn_then_status_then_stop_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, true).await.unwrap();
        let err = backend.status(&handle).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    async fn spawn_then_pause_then_resume_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();

        backend.pause(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Paused);

        backend.resume(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, false).await.unwrap();
    }

    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    #[tokio::test]
    async fn pause_after_external_process_kill_returns_process_not_running() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();

        backend.pause(&handle).await.unwrap();
        backend.resume(&handle).await.unwrap();

        let pid = {
            let instances = backend.instances.lock().await;
            instances
                .get(&handle)
                .expect("just-spawned instance must be registered")
                .process
                .pid()
        };

        // SAFETY: PID is from a just-spawned QEMU process we own; used to simulate crash.
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let err = backend.pause(&handle).await.unwrap_err();
        assert!(
            matches!(err, BackendError::ProcessNotRunning),
            "expected ProcessNotRunning, got {err:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    async fn log_stream_receives_real_process_output() {
        use futures_util::StreamExt;
        use tokio::time::{timeout, Duration};

        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let mut stream = backend.log_stream(&handle);

        let _ = timeout(Duration::from_secs(5), stream.next()).await;

        // Killing the process must close the stream (history is complete) —
        // BroadcastStream alone would hold it open forever.
        let mut instances = backend.instances.lock().await;
        let pid = instances.get_mut(&handle).unwrap().process.pid();
        drop(instances);
        // SAFETY: pid belongs to our own spawned child; SIGKILL is the one
        // signal that cannot be blocked or handled by qemu.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);

        match timeout(Duration::from_secs(5), async {
            let mut buffered = Vec::new();
            while let Some(line) = stream.next().await {
                buffered.push(line);
            }
        })
        .await
        {
            Ok(()) => {}
            Err(_) => panic!("log stream did not end after process death"),
        }

        backend.stop(&handle, false).await.unwrap();
    }

    #[test]
    fn cmdline_marker_matches_live_process_arg() {
        // A loop (not a single trailing command) defeats the shell's exec
        // optimization, so the argv with `process=` marker stays on the shell
        // process itself; `sleep 1` children live at most 1s and never outlive
        // the killed shell long enough to hold the test harness's stdout pipe.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "while true; do sleep 1; done",
                "sh",
                "process=my-instance",
            ])
            .spawn()
            .expect("sh must spawn");
        let pid = child.id();
        // /proc/<pid>/cmdline is empty between fork and exec — wait for argv to appear
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            if !cmdline.is_empty() {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sh never reached exec within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match verify_cmdline_marker(pid, "my-instance") {
            Ok(()) => {}
            Err(err) => panic!("marker verification failed: {err}"),
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn cmdline_marker_rejects_unrelated_process() {
        assert!(
            verify_cmdline_marker(std::process::id(), "no-such-instance").is_err(),
            "a process without the marker must be rejected"
        );
    }

    fn ensure_test_disk() {
        const DISK: &str = "/tmp/test-disk.qcow2";
        const VARS: &str = "test-vm_VARS.fd";
        let valid = std::fs::metadata(DISK)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        if !valid {
            let status = std::process::Command::new("qemu-img")
                .args(["create", "-f", "qcow2", DISK, "512M"])
                .status()
                .expect("qemu-img must be available for ignored backend tests");
            assert!(status.success(), "qemu-img create of the test disk failed");
        }
        if std::fs::metadata(VARS).is_err() {
            let status = std::process::Command::new("qemu-img")
                .args(["create", "-f", "raw", VARS, "4M"])
                .status()
                .expect("qemu-img must be available for ignored backend tests");
            assert!(
                status.success(),
                "qemu-img create of the VARS fixture failed"
            );
        }
        let _ = std::fs::File::create("/tmp/test.iso");
    }

    #[test]
    fn snapshot_node_name_strips_tmp_and_fits_qemu_limit() {
        let long_id = format!("{:032x}", 0xdead_beef_u64); // 32 hex chars, uuid-like
        let name = snapshot_node_name(&PathBuf::from(format!(
            "/i/disk.snapshots/.tmp-{long_id}.qcow2"
        )));
        assert_eq!(name, format!("snap-{}", &long_id[..24]));
        assert!(name.len() <= 31, "QEMU node names are capped at 31 chars");
        assert!(!name.contains(".tmp-"));
    }
}

#[cfg(test)]
mod package_argv_tests {
    use super::QemuBackend;
    use andler_core::package_manager::PackageManager;

    #[test]
    fn argv_heads_the_manager_binary() {
        let argv = QemuBackend::package_argv(PackageManager::Apt, true, "hello");
        assert_eq!(argv, vec!["apt-get", "install", "-y", "hello"]);
        let argv = QemuBackend::package_argv(PackageManager::Apt, false, "hello");
        assert_eq!(argv, vec!["apt-get", "remove", "-y", "hello"]);
        let argv = QemuBackend::package_argv(PackageManager::Pacman, true, "hello");
        assert_eq!(argv, vec!["pacman", "-S", "--noconfirm", "hello"]);
        let argv = QemuBackend::package_argv(PackageManager::Dnf, true, "hello");
        assert_eq!(argv, vec!["dnf", "install", "-y", "hello"]);
    }
}
