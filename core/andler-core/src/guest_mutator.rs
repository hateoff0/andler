use async_trait::async_trait;
use thiserror::Error;

/// A single mutation to apply inside a guest filesystem. Operations are
/// batched so offline backends can run one appliance session per batch
/// instead of one per file (a staging run touches hundreds of files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutatorOp {
    WriteFile {
        path: String,
        content: Vec<u8>,
    },
    UploadFile {
        path: String,
        host_path: std::path::PathBuf,
    },
    MkdirP {
        path: String,
    },
    CpA {
        src: String,
        dst: String,
    },
    Mv {
        src: String,
        dst: String,
    },
    RmRf {
        path: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    Symlink {
        target: String,
        link: String,
    },
}

#[derive(Debug, Error)]
pub enum MutatorError {
    #[error("guest mutation failed: {0}")]
    Io(String),
    #[error("guest path not found: {0}")]
    NotFound(String),
    #[error("guest mutation not supported: {0}")]
    Unsupported(String),
}

/// Uniform guest-filesystem mutation layer with two real backends: the
/// online `QgaMutator` (QEMU guest agent, zero root) and the offline
/// `GuestfsMutator` (libguestfs appliance, zero root). Both must produce
/// the same observable result for the same op batch — the conformance
/// suite in `guest_mutator::conformance` runs against each.
#[async_trait]
pub trait GuestMutator: Send + Sync {
    fn name(&self) -> &'static str;

    /// Applies a batch of mutations in one session. The batch contract is
    /// what lets the offline backend run a single appliance session per
    /// staging run instead of one per file.
    async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError>;

    /// Reads a file's full content as raw bytes.
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, MutatorError>;

    /// Whether the path exists in the guest (file or directory).
    async fn exists(&self, path: &str) -> Result<bool, MutatorError>;
}

/// Conformance suite shared by both real implementations. Each backend's
/// test module calls `run_conformance(&mutator)`; a behavioral drift
/// between `QgaMutator` and `GuestfsMutator` fails here, not in daemon
/// tests that fake the trait away. Public (not `cfg(test)`) so both
/// backend crates can link it into their own test binaries.
pub mod conformance {
    use super::*;

    pub async fn run_conformance<M: GuestMutator>(m: &M) {
        let dir = "/tmp/andler-mutator-conformance";
        m.apply(&[
            MutatorOp::RmRf {
                path: dir.to_string(),
            },
            MutatorOp::MkdirP {
                path: dir.to_string(),
            },
            MutatorOp::MkdirP {
                path: format!("{dir}/nested/deeper"),
            },
            MutatorOp::WriteFile {
                path: format!("{dir}/hello.txt"),
                content: b"hello world\n".to_vec(),
            },
            MutatorOp::Symlink {
                target: "hello.txt".to_string(),
                link: format!("{dir}/link.txt"),
            },
            MutatorOp::Chmod {
                path: format!("{dir}/hello.txt"),
                mode: 0o640,
            },
        ])
        .await
        .expect("apply batch must succeed");

        let content = m
            .read_file(&format!("{dir}/hello.txt"))
            .await
            .expect("read back must succeed");
        assert_eq!(content, b"hello world\n");

        let host_file = std::env::temp_dir().join("andler-mutator-upload.bin");
        std::fs::write(&host_file, b"uploaded\x00bytes").expect("write host fixture");
        m.apply(&[MutatorOp::UploadFile {
            path: format!("{dir}/uploaded.bin"),
            host_path: host_file.clone(),
        }])
        .await
        .expect("upload must succeed");
        assert_eq!(
            m.read_file(&format!("{dir}/uploaded.bin"))
                .await
                .expect("uploaded read back"),
            b"uploaded\x00bytes"
        );
        let _ = std::fs::remove_file(&host_file);

        m.apply(&[MutatorOp::CpA {
            src: format!("{dir}/hello.txt"),
            dst: format!("{dir}/copy.txt"),
        }])
        .await
        .expect("cp must succeed");
        assert_eq!(
            m.read_file(&format!("{dir}/copy.txt"))
                .await
                .expect("copy read must succeed"),
            b"hello world\n"
        );

        m.apply(&[MutatorOp::Mv {
            src: format!("{dir}/copy.txt"),
            dst: format!("{dir}/moved.txt"),
        }])
        .await
        .expect("mv must succeed");
        let err = m.read_file(&format!("{dir}/copy.txt")).await;
        assert!(err.is_err(), "source must be gone after mv");
        assert!(
            m.exists(&format!("{dir}/moved.txt")).await.unwrap(),
            "moved file must exist"
        );
        assert!(
            !m.exists(&format!("{dir}/copy.txt")).await.unwrap(),
            "source must be gone after mv"
        );

        m.apply(&[MutatorOp::RmRf {
            path: dir.to_string(),
        }])
        .await
        .expect("rm -rf must succeed");
        assert!(!m.exists(dir).await.unwrap(), "removed dir must not exist");
    }
}
