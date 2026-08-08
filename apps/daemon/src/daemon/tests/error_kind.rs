use super::*;
use andler_core::{BackendError, FsmError, InstanceEvent, InstanceState};
use andler_disk::DiskError;
use andler_store::StoreError;

fn id() -> InstanceId {
    InstanceId::new()
}

fn state() -> InstanceState {
    InstanceState::Stopped
}

fn io_err() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, "test io error")
}

#[test]
fn every_error_variant_maps_to_its_documented_kind() {
    let cases: Vec<(DaemonError, ErrorKind)> = vec![
        (
            DaemonError::InvalidConfig("bad toml".into()),
            ErrorKind::InvalidArgument,
        ),
        (DaemonError::InstanceNotFound(id()), ErrorKind::NotFound),
        (
            DaemonError::NoBackendRegistered(BackendKind::Qemu),
            ErrorKind::Unimplemented,
        ),
        (
            DaemonError::InvalidTransition(FsmError::InvalidTransition {
                from: InstanceState::Created,
                event: InstanceEvent::Stop,
            }),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::Backend(BackendError::NotImplemented {
                backend: "qemu",
                operation: "snapshot",
            }),
            ErrorKind::Unimplemented,
        ),
        (
            DaemonError::Backend(BackendError::HandleNotFound("qemu:x".into())),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::Backend(BackendError::ProcessNotRunning),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::Backend(BackendError::Io("qmp died".into())),
            ErrorKind::Internal,
        ),
        (
            DaemonError::Backend(BackendError::InvalidConfig {
                backend: "qemu",
                reason: "no render backend".into(),
            }),
            ErrorKind::Internal,
        ),
        (
            DaemonError::Disk(DiskError::SpawnFailed(io_err())),
            ErrorKind::Internal,
        ),
        (
            DaemonError::Disk(DiskError::InsufficientDiskSpace {
                path: std::path::PathBuf::from("/x"),
                required_bytes: 10,
                available_bytes: 4,
            }),
            ErrorKind::ResourceExhausted,
        ),
        (DaemonError::Firmware("no ovmf".into()), ErrorKind::Internal),
        (
            DaemonError::Io {
                path: std::path::PathBuf::from("/x"),
                source: io_err(),
            },
            ErrorKind::Internal,
        ),
        (
            DaemonError::Store(StoreError::NotFound(id())),
            ErrorKind::Internal,
        ),
        (
            DaemonError::InstanceNotRemovable(id(), state()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::InstanceNotClonable(id(), state()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::InstanceAlreadyStopped(id(), "it stopped".into()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::SharedBaseNotSupportedForLinuxVm(id()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::InstanceHasLiveClones(id(), vec![]),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::SnapshotNotFound {
                instance_id: id(),
                tag: "t".into(),
            },
            ErrorKind::NotFound,
        ),
        (
            DaemonError::SnapshotAlreadyExists {
                instance_id: id(),
                tag: "t".into(),
            },
            ErrorKind::AlreadyExists,
        ),
        (
            DaemonError::SnapshotOperationRequiresRunningInstance(id(), state()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::SnapshotLimitExceeded {
                instance_id: id(),
                current: 1,
                limit: 4,
            },
            ErrorKind::FailedPrecondition,
        ),
        (DaemonError::EmptyInstanceRef, ErrorKind::InvalidArgument),
        (
            DaemonError::ConfigIdMismatch {
                expected: id(),
                actual: id(),
            },
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::ConfigKindChanged(id()),
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::ConfigDiskPathChanged(id()),
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::MalformedInstanceRef("zzz".into()),
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::InstanceRefNotFound("zzz".into()),
            ErrorKind::NotFound,
        ),
        (
            DaemonError::AmbiguousInstanceId {
                prefix: "abc".into(),
                candidates: vec![id()],
            },
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::GuestAgentUnavailable {
                instance_id: id(),
                message: "qga down".into(),
            },
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::InvalidConfigKey("display.foo".into()),
            ErrorKind::InvalidArgument,
        ),
        (DaemonError::NotAndroid(id()), ErrorKind::FailedPrecondition),
        (
            DaemonError::InstanceMustBeStopped(id(), state()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::MissingOvmfVarsTemplate,
            ErrorKind::InvalidArgument,
        ),
        (
            DaemonError::HotplugRequiresRunningInstance(id(), state()),
            ErrorKind::FailedPrecondition,
        ),
        (
            DaemonError::DiskAlreadyAttached(id(), "disk.qcow2".into()),
            ErrorKind::AlreadyExists,
        ),
        (
            DaemonError::DiskNotAttached(id(), "disk.qcow2".into()),
            ErrorKind::NotFound,
        ),
        (
            DaemonError::NetworkNotAttached {
                instance_id: id(),
                index: 0,
                attached: 0,
            },
            ErrorKind::NotFound,
        ),
    ];

    for (err, expected) in cases {
        assert_eq!(
            err.kind(),
            expected,
            "kind() mismatch for {err:?} — update ErrorKind::kind() and docs/GRPC_API.md together"
        );
    }
}
