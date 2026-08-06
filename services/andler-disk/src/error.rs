use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiskError {
    #[error("failed to spawn qemu-img: {0}")]
    SpawnFailed(std::io::Error),

    #[error("qemu-img exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },

    #[error("backing file does not exist: {0}")]
    BackingFileNotFound(PathBuf),

    #[error("failed to parse qemu-img output: {0}")]
    ParseError(String),

    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("filesystem operation failed: {0}")]
    FileSystem(String),

    #[error("nbd setup failed: {0}")]
    NbdSetupFailed(String),

    #[error(
        "shrinking {path} from {current_size_bytes} to {requested_size_bytes} bytes requires \
         explicit confirmation (risk of data loss if the guest filesystem was not shrunk first)"
    )]
    ShrinkRequiresConfirmation {
        path: PathBuf,
        current_size_bytes: u64,
        requested_size_bytes: u64,
    },

    #[error("compact is not applicable to `{format}` disks (only qcow2 has reclaimable metadata): {path}")]
    CompactNotApplicable { path: PathBuf, format: String },

    #[error(
        "no supported package manager (apt/dnf/pacman) found in guest filesystem: {mount_point}"
    )]
    PackageManagerNotFound { mount_point: PathBuf },

    #[error("package `{package}` is already installed in guest filesystem")]
    AgentAlreadyInstalled { package: String },

    #[error("package `{package}` is not installed in guest filesystem")]
    AgentNotInstalled { package: String },

    #[error("QEMU guest agent is not available in instance {instance_id}")]
    GuestAgentUnavailable { instance_id: String },

    #[error(
        "not enough disk space at {path}: {available_bytes} bytes free, \
         ~{required_bytes} bytes needed"
    )]
    InsufficientDiskSpace {
        path: PathBuf,
        required_bytes: u64,
        available_bytes: u64,
    },
}
