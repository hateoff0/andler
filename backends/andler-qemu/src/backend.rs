use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use andler_core::{
    BackendError, BackendHandle, BackendStatus, HypervisorBackend, InstanceConfig, InstanceKind,
    InstanceState, LogLine, LogStreamSource, NetworkMode, RenderBackend, Resolution,
    ResourceMetrics,
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

    network_info: NetworkInfo,
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
}

impl QemuBackend {
    pub fn new() -> Self {
        QemuBackend {
            instances: Mutex::new(HashMap::new()),
            network_service: Arc::new(DefaultNetworkService::new()),
        }
    }

    fn handle_for(cfg: &InstanceConfig) -> BackendHandle {
        BackendHandle(format!("qemu:{}", cfg.id))
    }

    fn qmp_socket_path_for(cfg: &InstanceConfig) -> PathBuf {
        qmp_socket_dir().join(format!("{}.sock", cfg.id))
    }

    async fn ensure_qmp_connected(instance: &mut RunningInstance) -> Result<(), QmpError> {
        if instance.qmp_client.is_some() {
            return Ok(());
        }

        let client = QmpClient::connect(instance.process.qmp_socket_path()).await?;
        instance.qmp_client = Some(client);
        Ok(())
    }

    async fn guest_agent_client(instance: &RunningInstance) -> Result<QmpClient, BackendError> {
        let path = instance.process.qga_socket_path();
        QmpClient::connect_agent(&path)
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn diagnose_and_reset_qmp(instance: &mut RunningInstance) -> Option<BackendError> {
        instance.qmp_client = None;
        let alive = instance.process.is_alive().await.unwrap_or(false);
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

        let cmd_args: Vec<&str> = if install {
            match pkg_bin {
                "apt-get" => vec!["apt-get", "install", "-y", package],
                "dnf" => vec!["dnf", "install", "-y", package],
                "pacman" => vec!["pacman", "-S", "--noconfirm", package],
                _ => vec!["apt-get", "install", "-y", package],
            }
        } else {
            match pkg_bin {
                "apt-get" => vec!["apt-get", "remove", "-y", package],
                "dnf" => vec!["dnf", "remove", "-y", package],
                "pacman" => vec!["pacman", "-R", "--noconfirm", package],
                _ => vec!["apt-get", "remove", "-y", package],
            }
        };

        let pid = qga
            .guest_exec(cmd_args[0], &cmd_args[1..])
            .await
            .map_err(qmp_error_to_backend_error)?;

        let result = wait_for_guest_exec(&mut qga, pid, std::time::Duration::from_secs(60))
            .await
            .map_err(qmp_error_to_backend_error)?;

        if let Some(output) = result {
            tracing::info!(
                package = %package,
                install = %install,
                output = %output,
                "guest-exec completed"
            );
        }

        Ok(())
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

        let _ = tokio::fs::remove_file(&qmp_socket_path).await;

        let args = cmdline::build_args(cfg, &qmp_socket_path)?;

        let log_file_path = cfg.disk.path.parent().map(|dir| dir.join("qemu.log"));

        let network_info = match &cfg.network.mode {
            NetworkMode::Bridge { interface: bridge } => {
                let tap_iface = format!("tap{}", cfg.id);
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
        let process = QemuProcess::spawn(&args, qmp_socket_path, log_file_path).await;
        if let Err(err) = process {
            match &network_info {
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
            return Err(process_error_to_backend_error(err));
        }
        let process = process.unwrap();

        let mut instances = self.instances.lock().await;
        instances.insert(
            handle.clone(),
            RunningInstance {
                process,
                qmp_client: None,
                network_info,
            },
        );

        Ok(handle)
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
                if let Some(err) = Self::diagnose_and_reset_qmp(instance).await {
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
                if let Some(err) = Self::diagnose_and_reset_qmp(instance).await {
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

        Ok(())
    }

    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let alive = instance
            .process
            .is_alive()
            .await
            .map_err(process_error_to_backend_error)?;

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
        qmp.snapshot_save(DISK_DEVICE, tag)
            .await
            .map_err(qmp_error_to_backend_error)?;

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

    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine> {
        let handle = handle.clone();
        let stream = futures_util::stream::once(async move {
            let mut instances = self.instances.lock().await;
            let subscription = instances.get_mut(&handle).map(|i| {
                (
                    i.process.log_file_path().map(PathBuf::from),
                    i.process.subscribe_logs(),
                )
            });

            match subscription {
                Some((log_file_path, receiver)) => {
                    let live = tokio_stream::wrappers::BroadcastStream::new(receiver)
                        .filter_map(|item| async move { item.ok() });

                    let history = futures_util::stream::once(async move {
                        match log_file_path {
                            Some(path) => read_log_history(&path).await,
                            None => Vec::new(),
                        }
                    })
                    .flat_map(futures_util::stream::iter);

                    Box::pin(history.chain(live)) as BoxStream<'_, LogLine>
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

        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu,
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
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
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
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
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
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

    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
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
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn log_stream_receives_real_process_output() {
        use futures_util::StreamExt;
        use tokio::time::{timeout, Duration};

        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let mut stream = backend.log_stream(&handle);

        let _ = timeout(Duration::from_secs(5), stream.next()).await;

        backend.stop(&handle, false).await.unwrap();
    }
}
