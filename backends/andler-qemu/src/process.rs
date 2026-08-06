use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use andler_core::{LogLine, LogStreamSource, ResourceMetrics};
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
    child: Child,

    pid: u32,
    qmp_socket_path: PathBuf,

    log_file_path: Option<PathBuf>,

    log_sender: broadcast::Sender<LogLine>,

    metrics_sender: broadcast::Sender<ResourceMetrics>,

    _metrics_task: tokio::task::JoinHandle<()>,
}

impl QemuProcess {
    pub async fn spawn(
        args: &[String],
        qmp_socket_path: PathBuf,
        log_file_path: Option<PathBuf>,
    ) -> Result<Self, ProcessError> {
        let mut child = Command::new(QEMU_BINARY)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(ProcessError::SpawnFailed)?;

        let pid = child.id().expect("freshly spawned child must have a pid");

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

        let (metrics_sender, _) = broadcast::channel(METRICS_CHANNEL_CAPACITY);
        let ticks_per_sec = crate::metrics::ticks_per_second();
        let metrics_task = crate::metrics::spawn_metrics_poller(
            pid,
            ticks_per_sec,
            crate::metrics::DEFAULT_POLL_INTERVAL,
            metrics_sender.clone(),
        );

        Ok(QemuProcess {
            child,
            pid,
            qmp_socket_path,
            log_file_path,
            log_sender,
            metrics_sender,
            _metrics_task: metrics_task,
        })
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
                    tracing::warn!(pid, stream = stream_name, "{line}");
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

    pub async fn is_alive(&mut self) -> Result<bool, ProcessError> {
        match self.child.try_wait().map_err(ProcessError::Io)? {
            Some(_exit_status) => Ok(false),
            None => Ok(true),
        }
    }

    pub async fn terminate(&mut self) -> Result<(), ProcessError> {
        // SAFETY: PID is from our own spawned child process; SIGTERM is a standard signal.
        let kill_result = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) };
        if kill_result != 0 {
            return Err(ProcessError::Io(std::io::Error::last_os_error()));
        }

        match timeout(GRACEFUL_SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_exit_status)) => Ok(()),
            Ok(Err(io_err)) => Err(ProcessError::Io(io_err)),
            Err(_elapsed) => Err(ProcessError::GracefulShutdownTimedOut(
                GRACEFUL_SHUTDOWN_TIMEOUT,
            )),
        }
    }

    pub async fn force_kill(&mut self) -> Result<(), ProcessError> {
        self.child.kill().await.map_err(ProcessError::Io)
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

        let mut process = QemuProcess::spawn(&args, qmp_path, None).await.unwrap();
        assert!(process.is_alive().await.unwrap());

        process.terminate().await.unwrap();
        assert!(!process.is_alive().await.unwrap());
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

        let mut process = QemuProcess::spawn(&args, qmp_path, None).await.unwrap();
        process.force_kill().await.unwrap();
        assert!(!process.is_alive().await.unwrap());
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
