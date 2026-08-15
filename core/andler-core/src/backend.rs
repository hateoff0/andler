use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::config::{DiskConfig, InstanceConfig, NetworkConfig, Resolution};
use crate::error::BackendError;
use crate::fsm::InstanceState;
use crate::guest_mutator::GuestMutator;
use crate::InstanceKind;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendHandle(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub state: InstanceState,

    pub detail: Option<String>,

    /// True when this status reflects a guest-initiated, orderly shutdown (e.g. QMP
    /// reporting VmStatus::Shutdown) rather than the backend process disappearing
    /// unexpectedly. Consumers (see health_ops.rs) use this to route to a clean
    /// `Stopped` transition instead of treating it as a crash.
    pub clean_shutdown: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceMetrics {
    pub cpu_percent: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub disk_read_bytes_per_sec: Option<u64>,
    pub disk_write_bytes_per_sec: Option<u64>,
    pub net_rx_bytes_per_sec: Option<u64>,
    pub net_tx_bytes_per_sec: Option<u64>,

    pub vram_used_bytes: Option<u64>,

    pub vram_total_bytes: Option<u64>,

    pub gpu_load_percent: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStreamSource {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub source: LogStreamSource,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub tag: String,
    pub id: String,

    pub created_at: Option<String>,
}

#[async_trait]
pub trait HypervisorBackend: Send + Sync {
    fn name(&self) -> &'static str;

    fn supported_render_backends(&self) -> &[crate::config::RenderBackend];

    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>;

    /// Re-attaches to an instance whose process survived a daemon restart,
    /// returning the recovered handle and the VM's actual state. The backend
    /// must verify identity (never trust a pid); QEMU does so via the
    /// instance-owned QMP socket plus the `process=` cmdline marker.
    async fn adopt(
        &self,
        cfg: &InstanceConfig,
    ) -> Result<(BackendHandle, InstanceState), BackendError> {
        let _ = cfg;
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "adopt",
        })
    }

    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError>;

    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError>;

    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>;

    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>;

    /// Takes a live disk snapshot by switching the VM's disk graph to the
    /// external overlay at `layer_path` (created by the caller with its
    /// backing reference already pointing at the layer the head will become).
    /// The caller owns the file: it is the backend's job only to attach the
    /// overlay to the running graph, not to create or rename files.
    async fn snapshot(
        &self,
        handle: &BackendHandle,
        layer_path: &std::path::Path,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, layer_path, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot",
        })
    }

    async fn snapshot_restore(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, tag, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_restore",
        })
    }

    async fn snapshot_delete(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let _ = (handle, tag, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_delete",
        })
    }

    async fn snapshot_list(
        &self,
        handle: &BackendHandle,
    ) -> Result<Vec<SnapshotInfo>, BackendError> {
        let _ = handle;
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "snapshot_list",
        })
    }

    /// Attaches `disk` as hotplug device `index` (index into the instance's
    /// `extra_disks` list — the backend derives QEMU ids from it). The caller
    /// persists the device in the instance config after a successful attach.
    async fn attach_disk(
        &self,
        handle: &BackendHandle,
        disk: &DiskConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        let _ = (handle, disk, index);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "attach_disk",
        })
    }

    async fn detach_disk(&self, handle: &BackendHandle, index: usize) -> Result<(), BackendError> {
        let _ = (handle, index);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "detach_disk",
        })
    }

    /// Attaches `network` as hotplug device `index` (index into the instance's
    /// `extra_networks` list). Host-side taps/veths are created before the QMP
    /// hotplug and torn down again on failure or detach.
    async fn attach_network(
        &self,
        handle: &BackendHandle,
        network: &NetworkConfig,
        index: usize,
    ) -> Result<(), BackendError> {
        let _ = (handle, network, index);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "attach_network",
        })
    }

    async fn detach_network(
        &self,
        handle: &BackendHandle,
        index: usize,
    ) -> Result<(), BackendError> {
        let _ = (handle, index);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "detach_network",
        })
    }

    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>;

    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine>;

    /// Streams raw QMP events from the backend (QEMU only). The daemon
    /// relays these onto its event bus as `DaemonEvent::Qmp`; the default
    /// returns `None` for backends without a QMP channel.
    fn subscribe_qmp_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::events::QmpEventRecord>> {
        None
    }

    /// Stream that yields once per process exit (the process is gone after
    /// the first item). Defaults to an empty stream for backends that do not
    /// supervise an external process; the QEMU backend implements it via
    /// pidfd wait.
    fn process_exit_stream(&self, _handle: &BackendHandle) -> BoxStream<'_, ()> {
        Box::pin(futures_util::stream::empty())
    }

    async fn is_guest_agent_available(&self, handle: &BackendHandle) -> Result<bool, BackendError> {
        let _ = handle;
        Ok(false)
    }

    async fn set_guest_display_resolution(
        &self,
        handle: &BackendHandle,
        resolution: &Resolution,
        kind: InstanceKind,
    ) -> Result<(), BackendError> {
        let _ = (handle, resolution, kind);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "set_guest_display_resolution",
        })
    }

    async fn guest_exec_install(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        let _ = (handle, package);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "guest_exec_install",
        })
    }

    async fn guest_exec_remove(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        let _ = (handle, package);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "guest_exec_remove",
        })
    }

    async fn guest_check_binary_installed(
        &self,
        handle: &BackendHandle,
        binary_path: &str,
    ) -> Result<bool, BackendError> {
        let _ = (handle, binary_path);
        Ok(false)
    }

    /// Runs an arbitrary command in the guest through the guest agent and
    /// returns its exit code plus captured stdout/stderr.
    async fn guest_exec_command(
        &self,
        handle: &BackendHandle,
        argv: &[String],
        timeout: Option<std::time::Duration>,
    ) -> Result<GuestExecOutput, BackendError> {
        let _ = (handle, argv, timeout);
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "guest_exec_command",
        })
    }

    /// Returns an online `GuestMutator` bound to the running guest's agent.
    /// The returned mutator owns its QGA connection (chardevs serve exactly
    /// one client), so callers must not interleave other guest-exec traffic
    /// while it lives.
    async fn guest_mutator(
        &self,
        handle: &BackendHandle,
    ) -> Result<Box<dyn GuestMutator>, BackendError> {
        let _ = handle;
        Err(BackendError::NotImplemented {
            backend: self.name(),
            operation: "guest_mutator",
        })
    }
}

/// Result of a guest-exec command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}
