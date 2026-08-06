use std::path::PathBuf;

use andler_core::{BackendError, BackendKind, InstanceId, InstanceState};
use andler_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("instance {0:?} not found")]
    InstanceNotFound(InstanceId),

    #[error("no backend registered for {0:?}")]
    NoBackendRegistered(BackendKind),

    #[error("invalid state transition: {0}")]
    InvalidTransition(#[from] andler_core::FsmError),

    #[error("backend error: {0}")]
    Backend(#[from] BackendError),

    #[error("disk error: {0}")]
    Disk(#[from] andler_disk::DiskError),

    #[error("firmware error: {0}")]
    Firmware(String),

    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("cannot remove instance {0:?}: it is in non-terminal state {1:?}; stop it first")]
    InstanceNotRemovable(InstanceId, InstanceState),

    #[error(
        "cannot clone/export instance {0:?}: it is in non-terminal state {1:?}; stop it first"
    )]
    InstanceNotClonable(InstanceId, InstanceState),

    #[error(
        "instance {0:?} is not running (it already stopped: {1}); nothing to do — \
         see `andler status` for details, or `andler start` to run it again"
    )]
    InstanceAlreadyStopped(InstanceId, String),

    #[error("shared-base clone is not supported for LinuxVm: {0:?}")]
    SharedBaseNotSupportedForLinuxVm(InstanceId),

    #[error("cannot purge instance {0:?}: it has live linked clones {1:?}; remove them first")]
    InstanceHasLiveClones(InstanceId, Vec<InstanceId>),

    #[error("snapshot {tag:?} not found for instance {instance_id:?}")]
    SnapshotNotFound {
        instance_id: InstanceId,
        tag: String,
    },

    #[error("snapshot {tag:?} already exists for instance {instance_id:?}")]
    SnapshotAlreadyExists {
        instance_id: InstanceId,
        tag: String,
    },

    #[error("snapshot operation requires running instance {0:?}, but state is {1:?}")]
    SnapshotOperationRequiresRunningInstance(InstanceId, InstanceState),

    #[error(
        "instance {instance_id:?} already has {current} snapshots (limit {limit}); delete one before creating another"
    )]
    SnapshotLimitExceeded {
        instance_id: InstanceId,
        current: usize,
        limit: usize,
    },

    #[error("instance reference must not be empty")]
    EmptyInstanceRef,

    #[error("cannot update instance {expected:?}: config has a different id {actual:?}")]
    ConfigIdMismatch {
        expected: InstanceId,
        actual: InstanceId,
    },

    #[error(
        "cannot change instance {0:?} kind (LinuxVm <-> AndroidVm) via edit; \
         recreate the instance instead"
    )]
    ConfigKindChanged(InstanceId),

    #[error("cannot change instance {0:?} disk path via edit; use `andler disk` commands instead")]
    ConfigDiskPathChanged(InstanceId),

    #[error("instance reference {0:?} is not a valid UUID or hex prefix")]
    MalformedInstanceRef(String),

    #[error("no instance found matching {0:?}")]
    InstanceRefNotFound(String),

    #[error("instance reference {prefix:?} is ambiguous, matches: {candidates:?}")]
    AmbiguousInstanceId {
        prefix: String,
        candidates: Vec<InstanceId>,
    },

    #[error("guest agent unavailable for instance {instance_id:?}: {message}")]
    GuestAgentUnavailable {
        instance_id: InstanceId,
        message: String,
    },

    #[error("invalid config key: {0:?}")]
    InvalidConfigKey(String),

    #[error("instance {0:?} is not an Android VM")]
    NotAndroid(InstanceId),

    #[error("instance {0:?} must be stopped (currently {1:?}) to change config")]
    InstanceMustBeStopped(InstanceId, InstanceState),

    #[error("Android requires UEFI/OVMF. Provide an OVMF_VARS template.")]
    MissingOvmfVarsTemplate,
}
