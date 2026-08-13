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

    #[error("instance {0:?} supervisor is gone; the daemon is likely shutting down")]
    InstanceSupervisorGone(InstanceId),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error(
        "cannot migrate instance {instance_id}: {file_path} already exists and differs from \
         the config stored in the database; keeping both would be ambiguous — move or delete \
         one of them and restart andlerd"
    )]
    ConfigMigrationConflict {
        instance_id: InstanceId,
        file_path: PathBuf,
    },

    #[error("config file {path} is invalid: {message}")]
    ConfigFileInvalid { path: PathBuf, message: String },

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

    #[error("snapshot {tag:?} not found for instance {instance_id}")]
    SnapshotNotFound {
        instance_id: InstanceId,
        tag: String,
    },

    #[error("snapshot {tag:?} already exists for instance {instance_id}")]
    SnapshotAlreadyExists {
        instance_id: InstanceId,
        tag: String,
    },

    #[error("snapshot operation requires running instance {0:?}, but state is {1:?}")]
    SnapshotOperationRequiresRunningInstance(InstanceId, InstanceState),

    #[error(
        "instance {instance_id} already has {current} snapshots (limit {limit}); delete one before creating another"
    )]
    SnapshotLimitExceeded {
        instance_id: InstanceId,
        current: usize,
        limit: usize,
    },

    #[error(
        "snapshot of instance {instance_id} requires a qcow2 disk (current format: {format}); \
         external snapshots need overlay support — convert the disk to qcow2 first"
    )]
    SnapshotRequiresQcow2 {
        instance_id: InstanceId,
        format: String,
    },

    #[error(
        "snapshot layer file {path} of instance {instance_id} is missing; \
         the chain may have been tampered with — restart the daemon to reconcile"
    )]
    SnapshotLayerMissing {
        instance_id: InstanceId,
        path: PathBuf,
    },

    #[error(
        "snapshot {tag:?} of instance {instance_id} is a legacy internal qcow2 snapshot: \
         restoring it is not supported; create a new snapshot instead"
    )]
    SnapshotInternalNotRestorable {
        instance_id: InstanceId,
        tag: String,
    },

    #[error(
        "snapshot {tag:?} of instance {instance_id} is on archived branch {branch:?}; \
         restoring it would destroy the branch's newer layers — pass --branch to switch to it instead"
    )]
    RestoreTargetOnArchivedBranch {
        instance_id: InstanceId,
        tag: String,
        branch: String,
    },

    #[error(
        "cannot restore snapshot {tag:?} of instance {instance_id}: the instance has live \
         linked clones {} whose disks derive from the current disk; removing their source \
         would orphan them — remove the clones first",
        short_ids(clones)
    )]
    RestoreWouldBreakClones {
        instance_id: InstanceId,
        tag: String,
        clones: Vec<InstanceId>,
    },

    #[error(
        "cannot delete snapshot {tag:?} of instance {instance_id}: the layer is read by \
         linked clones {}; deleting it would break their disk chains — remove the clones \
         first",
        short_ids(clones)
    )]
    DeleteWouldBreakClones {
        instance_id: InstanceId,
        tag: String,
        clones: Vec<InstanceId>,
    },

    #[error(
        "cannot delete snapshot {tag:?} of instance {instance_id}: the layer has no backing \
         file to merge its data into (it is the chain's base)"
    )]
    CannotDeleteBaseLayer {
        instance_id: InstanceId,
        tag: String,
    },

    #[error(
        "instance {instance_id} already runs operation {active_op_id:?} ({active_kind:?}); \
         wait for it to finish or cancel it with `andler op cancel {active_op_id}`"
    )]
    OperationAlreadyRunning {
        instance_id: InstanceId,
        active_op_id: String,
        active_kind: andler_core::OperationKind,
    },

    #[error("operation {0:?} was cancelled")]
    OperationCancelled(String),

    #[error("no active operation with id {0:?}; `andler op list` shows the running ones")]
    OperationNotFound(String),

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

    #[error("instance reference {0:?} is not a valid ID (64 hex chars) or hex prefix")]
    MalformedInstanceRef(String),

    #[error("no instance found matching {0:?}")]
    InstanceRefNotFound(String),

    #[error("instance reference {prefix:?} is ambiguous, matches: {candidates:?}")]
    AmbiguousInstanceId {
        prefix: String,
        candidates: Vec<InstanceId>,
    },

    #[error("guest agent unavailable for instance {instance_id}: {message}")]
    GuestAgentUnavailable {
        instance_id: InstanceId,
        message: String,
    },

    #[error("invalid config key: {0:?}")]
    InvalidConfigKey(String),

    #[error("instance {0:?} is not an Android VM")]
    NotAndroid(InstanceId),

    #[error(
        "instance {0:?} must be stopped (currently {1:?}) to perform this operation; stop it first"
    )]
    InstanceMustBeStopped(InstanceId, InstanceState),

    #[error("Android requires UEFI/OVMF. Provide an OVMF_VARS template.")]
    MissingOvmfVarsTemplate,

    #[error(
        "hotplug requires instance {0} to be running or paused (currently {1:?}); \
         start it first"
    )]
    HotplugRequiresRunningInstance(InstanceId, InstanceState),

    #[error("disk {1:?} is already attached to instance {0:?}")]
    DiskAlreadyAttached(InstanceId, PathBuf),

    #[error(
        "disk {1:?} is not attached to instance {0:?}; \
         see `andler config <id>` for the extra_disks list"
    )]
    DiskNotAttached(InstanceId, PathBuf),

    #[error(
        "network device {index} is not attached to instance {instance_id} \
         (only {attached} extra network(s) present); \
         see `andler config <id>` for the extra_networks list"
    )]
    NetworkNotAttached {
        instance_id: InstanceId,
        index: usize,
        attached: usize,
    },
}

/// Machine-readable category of a `DaemonError`, independent of its
/// human-readable text. `DaemonError::kind()` is the single exhaustive match
/// over all variants; the gRPC mapping and any future error metrics key off
/// this, so a new variant can never silently fall through to `internal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidArgument,
    NotFound,
    AlreadyExists,
    FailedPrecondition,
    Unimplemented,
    ResourceExhausted,
    Internal,
}

impl DaemonError {
    pub(crate) fn from_config_key_error(err: andler_core::config::ConfigKeyError) -> DaemonError {
        use andler_core::config::ConfigKeyError;
        match err {
            ConfigKeyError::Unknown(key) => DaemonError::InvalidConfigKey(format!(
                "unknown config key {key:?}; see `andler config --help` or docs/API.md \
                 for the list of valid keys"
            )),
            ConfigKeyError::Immutable { key, reason } => {
                DaemonError::InvalidConfigKey(format!("cannot change {key:?}: {reason}"))
            }
            ConfigKeyError::InvalidValue { key, value, reason } => DaemonError::InvalidConfigKey(
                format!("invalid value {value:?} for {key:?}: {reason}"),
            ),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        match self {
            DaemonError::InvalidConfig(_) => ErrorKind::InvalidArgument,
            DaemonError::InstanceNotFound(_) => ErrorKind::NotFound,
            DaemonError::NoBackendRegistered(_) => ErrorKind::Unimplemented,
            DaemonError::InvalidTransition(_) => ErrorKind::FailedPrecondition,
            DaemonError::Backend(BackendError::NotImplemented { .. }) => ErrorKind::Unimplemented,
            DaemonError::Backend(
                BackendError::HandleNotFound(_) | BackendError::ProcessNotRunning,
            ) => ErrorKind::FailedPrecondition,
            DaemonError::Disk(andler_disk::DiskError::InsufficientDiskSpace { .. }) => {
                ErrorKind::ResourceExhausted
            }
            DaemonError::Backend(_)
            | DaemonError::Disk(_)
            | DaemonError::Io { .. }
            | DaemonError::Firmware(_)
            | DaemonError::Store(_)
            | DaemonError::InstanceSupervisorGone(_) => ErrorKind::Internal,
            DaemonError::InstanceNotRemovable(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::ConfigMigrationConflict { .. } => ErrorKind::FailedPrecondition,
            DaemonError::ConfigFileInvalid { .. } => ErrorKind::InvalidArgument,
            DaemonError::InstanceNotClonable(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::InstanceAlreadyStopped(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::SharedBaseNotSupportedForLinuxVm(_) => ErrorKind::FailedPrecondition,
            DaemonError::InstanceHasLiveClones(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::SnapshotNotFound { .. } => ErrorKind::NotFound,
            DaemonError::SnapshotAlreadyExists { .. } => ErrorKind::AlreadyExists,
            DaemonError::SnapshotOperationRequiresRunningInstance(_, _) => {
                ErrorKind::FailedPrecondition
            }
            DaemonError::SnapshotLimitExceeded { .. } => ErrorKind::FailedPrecondition,
            DaemonError::SnapshotRequiresQcow2 { .. } => ErrorKind::FailedPrecondition,
            DaemonError::SnapshotLayerMissing { .. } => ErrorKind::NotFound,
            DaemonError::SnapshotInternalNotRestorable { .. } => ErrorKind::FailedPrecondition,
            DaemonError::RestoreTargetOnArchivedBranch { .. } => ErrorKind::FailedPrecondition,
            DaemonError::RestoreWouldBreakClones { .. } => ErrorKind::FailedPrecondition,
            DaemonError::DeleteWouldBreakClones { .. } => ErrorKind::FailedPrecondition,
            DaemonError::CannotDeleteBaseLayer { .. } => ErrorKind::FailedPrecondition,
            DaemonError::OperationAlreadyRunning { .. } => ErrorKind::FailedPrecondition,
            DaemonError::OperationCancelled(_) => ErrorKind::FailedPrecondition,
            DaemonError::OperationNotFound(_) => ErrorKind::NotFound,
            DaemonError::EmptyInstanceRef => ErrorKind::InvalidArgument,
            DaemonError::ConfigIdMismatch { .. } => ErrorKind::InvalidArgument,
            DaemonError::ConfigKindChanged(_) => ErrorKind::InvalidArgument,
            DaemonError::ConfigDiskPathChanged(_) => ErrorKind::InvalidArgument,
            DaemonError::MalformedInstanceRef(_) => ErrorKind::InvalidArgument,
            DaemonError::InstanceRefNotFound(_) => ErrorKind::NotFound,
            DaemonError::AmbiguousInstanceId { .. } => ErrorKind::InvalidArgument,
            DaemonError::GuestAgentUnavailable { .. } => ErrorKind::FailedPrecondition,
            DaemonError::InvalidConfigKey(_) => ErrorKind::InvalidArgument,
            DaemonError::NotAndroid(_) => ErrorKind::FailedPrecondition,
            DaemonError::InstanceMustBeStopped(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::MissingOvmfVarsTemplate => ErrorKind::InvalidArgument,
            DaemonError::HotplugRequiresRunningInstance(_, _) => ErrorKind::FailedPrecondition,
            DaemonError::DiskAlreadyAttached(_, _) => ErrorKind::AlreadyExists,
            DaemonError::DiskNotAttached(_, _) => ErrorKind::NotFound,
            DaemonError::NetworkNotAttached { .. } => ErrorKind::NotFound,
        }
    }
}

/// Compact, human-friendly instance ids for error text: the 8-hex prefix,
/// like the CLI's partial-id resolution. The full 64-hex Debug rendering of
/// an id alone can exceed the 384-char gRPC message truncation and swallow
/// the actionable tail of the message.
fn short_ids(ids: &[InstanceId]) -> String {
    let short: Vec<String> = ids
        .iter()
        .map(|id| id.to_string().chars().take(8).collect())
        .collect();
    if short.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", short.join(", "))
    }
}
