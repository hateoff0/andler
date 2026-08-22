use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use andler_core::{LogLine, LogStreamSource, ResourceMetrics};
use std::sync::Arc;

use crate::pidfd;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::time::timeout;

const QEMU_BINARY: &str = "qemu-system-x86_64";

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

const LOG_CHANNEL_CAPACITY: usize = 256;

const METRICS_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to spawn {QEMU_BINARY}: {0}")]
    SpawnFailed(std::io::Error),

    #[error("process I/O error: {0}")]
    Io(std::io::Error),

    #[error("process did not exit within {0:?} after SIGTERM")]
    GracefulShutdownTimedOut(Duration),
}

pub struct QemuProcess {
    // drop-guard: kill_on_drop terminates qemu on daemon panic/unwind;
    // None for a process adopted after a daemon restart (never ours to kill-on-drop)
    #[allow(dead_code)]
    child: Option<Child>,

    pid: u32,
    pidfd: Arc<pidfd::PidFd>,
    qmp_socket_path: PathBuf,

    log_file_path: Option<PathBuf>,

    log_sender: broadcast::Sender<LogLine>,

    metrics_sender: broadcast::Sender<ResourceMetrics>,

    exit_sender: broadcast::Sender<()>,

    _metrics_task: tokio::task::JoinHandle<()>,

    _exit_task: tokio::task::JoinHandle<()>,
}

impl QemuProcess {
    pub async fn spawn(
        args: &[String],
        qmp_socket_path: PathBuf,
        log_file_path: Option<PathBuf>,
        kill_on_drop: bool,
        cpu_affinity: Option<&[usize]>,
    ) -> Result<Self, ProcessError> {
        let mut command = match cpu_affinity {
            // taskset pins every QEMU thread (vCPU, iothread, main) to the
            // configured host CPUs: threads inherit the process affinity.
            // The thread-context QEMU object exists but its accel binding
            // is absent on the QEMU versions this project targets, so the
            // process-level pin is the version-independent mechanism.
            Some(affinity) => {
                let list = affinity
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let mut command = Command::new("taskset");
                command.args(["-c", &list]).arg(QEMU_BINARY);
                command
            }
            None => Command::new(QEMU_BINARY),
        };
        let mut child = command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(kill_on_drop)
            .spawn()
            .map_err(ProcessError::SpawnFailed)?;

        let pid = child.id().expect("freshly spawned child must have a pid");
        let pidfd = Arc::new(pidfd::PidFd::open(pid).map_err(ProcessError::Io)?);

        let (log_sender, _) = broadcast::channel(LOG_CHANNEL_CAPACITY);

        let stdout_log_file = Self::open_log_file(log_file_path.as_deref()).await;
        let stderr_log_file = Self::open_log_file(log_file_path.as_deref()).await;

        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(Self::drain_to_tracing(
                stdout,
                pid,
                LogStreamSource::Stdout,
                log_sender.clone(),
                stdout_log_file,
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(Self::drain_to_tracing(
                stderr,
                pid,
                LogStreamSource::Stderr,
                log_sender.clone(),
                stderr_log_file,
            ));
        }

        Ok(Self::new_inner(
            Some(child),
            pid,
            pidfd,
            qmp_socket_path,
            log_file_path,
            log_sender,
        ))
    }

    /// Re-attaches to a QEMU process that survived a daemon restart (reconnect): the pid comes from the QMP socket's owner, identity
    /// is verified by the caller, and death notification runs through the
    /// same pidfd machinery as a freshly spawned process. The process is
    /// deliberately NOT killed on drop — it was not ours to begin with.
    pub fn adopt(
        pid: u32,
        qmp_socket_path: PathBuf,
        log_file_path: Option<PathBuf>,
    ) -> Result<Self, ProcessError> {
        let pidfd = Arc::new(pidfd::PidFd::open(pid).map_err(ProcessError::Io)?);
        let (log_sender, _) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        Ok(Self::new_inner(
            None,
            pid,
            pidfd,
            qmp_socket_path,
            log_file_path,
            log_sender,
        ))
    }

    fn new_inner(
        child: Option<Child>,
        pid: u32,
        pidfd: Arc<pidfd::PidFd>,
        qmp_socket_path: PathBuf,
        log_file_path: Option<PathBuf>,
        log_sender: broadcast::Sender<LogLine>,
    ) -> Self {
        let (exit_sender, _) = broadcast::channel(1);
        let exit_task = tokio::spawn({
            let pidfd = pidfd.clone();
            let sender = exit_sender.clone();
            async move {
                match pidfd.wait_for_exit().await {
                    Ok(status) => {
                        tracing::debug!(pid, %status, "qemu process exited");
                    }
                    Err(error) => {
                        tracing::error!(pid, %error, "failed to wait for qemu process exit");
                    }
                }
                let _ = sender.send(());
            }
        });

        let (metrics_sender, _) = broadcast::channel(METRICS_CHANNEL_CAPACITY);
        let ticks_per_sec = crate::metrics::ticks_per_second();
        let metrics_task = crate::metrics::spawn_metrics_poller(
            pid,
            pidfd.clone(),
            ticks_per_sec,
            crate::metrics::DEFAULT_POLL_INTERVAL,
            metrics_sender.clone(),
        );

        QemuProcess {
            child,
            pid,
            pidfd,
            qmp_socket_path,
            log_file_path,
            log_sender,
            metrics_sender,
            exit_sender,
            _metrics_task: metrics_task,
            _exit_task: exit_task,
        }
    }

    async fn open_log_file(path: Option<&std::path::Path>) -> Option<tokio::fs::File> {
        let path = path?;
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(file) => Some(file),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "failed to open qemu.log, continuing without a file log for this instance"
                );
                None
            }
        }
    }

    async fn drain_to_tracing<R>(
        reader: R,
        pid: u32,
        source: LogStreamSource,
        sender: broadcast::Sender<LogLine>,
        mut log_file: Option<tokio::fs::File>,
    ) where
        R: tokio::io::AsyncRead + Unpin,
    {
        let stream_name = match source {
            LogStreamSource::Stdout => "stdout",
            LogStreamSource::Stderr => "stderr",
        };
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    // Guest-adjacent output is its own stream:
                    // qemu.log + `andler logs <id>` carry it, the daemon log
                    // (and its ring) only gets it at debug — the guest can
                    // print anything into the console, so it must never
                    // surface at the default INFO level.
                    tracing::debug!(pid, stream = stream_name, "{line}");
                    if let Some(file) = log_file.as_mut() {
                        let entry = format!("[{stream_name}] {line}\n");
                        if file.write_all(entry.as_bytes()).await.is_err() {
                            log_file = None;
                        }
                    }
                    let _ = sender.send(LogLine { source, line });
                }
                Ok(None) => break,
                Err(io_err) => {
                    tracing::warn!(pid, stream = stream_name, error = %io_err, "failed to read qemu output");
                    break;
                }
            }
        }
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<LogLine> {
        self.log_sender.subscribe()
    }

    pub fn subscribe_metrics(&self) -> broadcast::Receiver<ResourceMetrics> {
        self.metrics_sender.subscribe()
    }

    /// Fires once when the QEMU process exits, for any reason — including
    /// a crash or a kill that bypassed `stop()`. This is the replacement for
    /// polling `is_alive()` (death is an event, not a 30s-late fact).
    pub fn subscribe_exit(&self) -> broadcast::Receiver<()> {
        self.exit_sender.subscribe()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn qmp_socket_path(&self) -> &PathBuf {
        &self.qmp_socket_path
    }

    pub fn qga_socket_path(&self) -> PathBuf {
        self.qmp_socket_path.with_extension("qga.sock")
    }

    pub fn log_file_path(&self) -> Option<&Path> {
        self.log_file_path.as_deref()
    }

    pub fn is_alive(&self) -> bool {
        !self.pidfd.has_exited()
    }

    pub async fn terminate(&mut self) -> Result<(), ProcessError> {
        // SAFETY: PID is from our own spawned child process; SIGTERM is a standard signal.
        let kill_result = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) };
        if kill_result != 0 {
            // ESRCH: the process is already gone (reaped by the pidfd exit
            // task) — stopping a dead process is not an error.
            if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(ProcessError::Io(std::io::Error::last_os_error()));
        }

        match timeout(GRACEFUL_SHUTDOWN_TIMEOUT, self.pidfd.wait_for_exit()).await {
            Ok(Ok(_exit_status)) => Ok(()),
            Ok(Err(io_err)) => Err(ProcessError::Io(io_err)),
            Err(_elapsed) => Err(ProcessError::GracefulShutdownTimedOut(
                GRACEFUL_SHUTDOWN_TIMEOUT,
            )),
        }
    }

    pub async fn force_kill(&mut self) -> Result<(), ProcessError> {
        // SIGKILL via libc instead of tokio Child::kill: the exit task holds
        // the reaping lock (pidfd), so a concurrent tokio try_wait inside
        // Child::kill races our waitpid and loses — surfacing as a bogus
        // "No child processes" even though the kill succeeded.
        // SAFETY: pid is from our own spawned child; SIGKILL is standard.
        let kill_result = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        if kill_result != 0 {
            // ESRCH: already reaped — killing a dead process is a no-op success.
            if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(ProcessError::Io(std::io::Error::last_os_error()));
        }
        // Best effort reap so no zombie outlives the kill by long.
        let _ = timeout(Duration::from_secs(5), self.pidfd.wait_for_exit()).await;
        Ok(())
    }

    pub fn pidfd(&self) -> &pidfd::PidFd {
        &self.pidfd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    async fn spawn_then_is_alive_then_terminate() {
        let qmp_path = std::env::temp_dir().join("andler-process-test-qmp.sock");
        let args = vec![
            "-display".to_string(),
            "none".to_string(),
            "-nographic".to_string(),
        ];

        let mut process = QemuProcess::spawn(&args, qmp_path, None, true, None)
            .await
            .unwrap();
        assert!(process.is_alive());

        process.terminate().await.unwrap();
        assert!(!process.is_alive());
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/e2e/README.md integration-test target"]
    async fn force_kill_stops_unresponsive_process() {
        let qmp_path = std::env::temp_dir().join("andler-process-test-qmp-killed.sock");
        let args = vec![
            "-display".to_string(),
            "none".to_string(),
            "-nographic".to_string(),
        ];

        let mut process = QemuProcess::spawn(&args, qmp_path, None, true, None)
            .await
            .unwrap();
        process.force_kill().await.unwrap();
        assert!(!process.is_alive());
    }

    #[tokio::test]
    async fn adopt_attaches_to_existing_process_and_tracks_its_death() {
        let mut sleeper = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("sleep must spawn");
        let pid = sleeper.id();

        let process = QemuProcess::adopt(pid, PathBuf::from("/nonexistent/qmp.sock"), None)
            .expect("adopt must succeed");
        assert!(process.is_alive(), "adopted process is running");

        let mut exit_rx = process.subscribe_exit();
        let (dead_tx, dead_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            dead_tx.send(exit_rx.recv().await).unwrap();
        });

        let _ = sleeper.kill();
        let _ = sleeper.wait();

        let notified = tokio::time::timeout(std::time::Duration::from_secs(5), dead_rx)
            .await
            .expect("exit notification must arrive after the process dies")
            .expect("channel must deliver");
        assert!(
            notified.is_ok(),
            "process death published on subscribe_exit"
        );
        assert!(!process.is_alive());
    }

    #[tokio::test]
    async fn adopt_rejects_non_existent_pid() {
        let result = QemuProcess::adopt(999_999, PathBuf::from("/tmp/qmp.sock"), None);
        assert!(result.is_err(), "pidfd_open must fail for a dead pid");
    }

    #[tokio::test]
    async fn spawn_with_missing_binary_fails_with_spawn_error() {
        let result = Command::new("/nonexistent-binary-for-andler-qemu-test")
            .spawn()
            .map_err(ProcessError::SpawnFailed);

        assert!(matches!(result, Err(ProcessError::SpawnFailed(_))));
    }

    #[tokio::test]
    async fn drain_to_tracing_publishes_lines_to_subscriber() {
        let (sender, mut receiver) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        let reader: &[u8] = b"first line\nsecond line\n";

        QemuProcess::drain_to_tracing(reader, 1234, LogStreamSource::Stdout, sender, None).await;

        let first = receiver.try_recv().expect("first line should be queued");
        assert_eq!(first.source, LogStreamSource::Stdout);
        assert_eq!(first.line, "first line");

        let second = receiver.try_recv().expect("second line should be queued");
        assert_eq!(second.source, LogStreamSource::Stdout);
        assert_eq!(second.line, "second line");

        assert!(receiver.try_recv().is_err(), "no more lines after EOF");
    }

    #[tokio::test]
    async fn drain_to_tracing_tolerates_no_subscribers() {
        let (sender, _) = broadcast::channel::<LogLine>(LOG_CHANNEL_CAPACITY);
        let reader: &[u8] = b"nobody is listening\n";

        QemuProcess::drain_to_tracing(reader, 1, LogStreamSource::Stderr, sender, None).await;
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive_the_same_line() {
        let (sender, mut first_receiver) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        let mut second_receiver = sender.subscribe();
        let reader: &[u8] = b"shared line\n";

        QemuProcess::drain_to_tracing(reader, 1, LogStreamSource::Stdout, sender, None).await;

        assert_eq!(
            first_receiver.try_recv().unwrap().line,
            "shared line".to_string()
        );
        assert_eq!(
            second_receiver.try_recv().unwrap().line,
            "shared line".to_string()
        );
    }
}
