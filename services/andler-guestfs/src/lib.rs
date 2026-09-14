use std::path::{Path, PathBuf};

use andler_core::{GuestMutator, MutatorError, MutatorOp};
use async_trait::async_trait;

/// How long one appliance session may run. Starting the appliance on a busy
/// host takes seconds; a batch that stages hundreds of files takes a while
/// longer. The bound exists so a stuck appliance becomes an error instead of a
/// hang.
const DEFAULT_GUESTFS_TIMEOUT_SECS: u64 = 300;
const MIN_GUESTFS_TIMEOUT_SECS: u64 = 30;

fn guestfish_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(andler_core::timeout::env_secs(
        "ANDLERD_GUESTFS_TIMEOUT_SECS",
        DEFAULT_GUESTFS_TIMEOUT_SECS,
        MIN_GUESTFS_TIMEOUT_SECS,
    ))
}

/// Offline `GuestMutator` over the libguestfs appliance (guestfish).
/// The appliance boots its own unprivileged QEMU, mounts the guest image
/// with exclusive locking (qemu image lock — the same guarantee the old
/// NbdGuard flock provided, without our own code), and needs zero root.
/// One appliance session per `apply` batch: a staging run that touches
/// hundreds of files is one guestfish invocation, not hundreds.
pub struct GuestfsMutator {
    disk: PathBuf,
    /// Explicit `guestfish -m` mount spec (`/dev/sda:/`), used when the
    /// disk has no inspectable OS (conformance images); `None` uses `-i`.
    mount: Option<String>,
}

impl GuestfsMutator {
    pub fn new(disk: PathBuf) -> Self {
        GuestfsMutator { disk, mount: None }
    }

    pub fn with_mount(disk: PathBuf, mount: String) -> Self {
        GuestfsMutator {
            disk,
            mount: Some(mount),
        }
    }

    async fn run_guestfish(
        &self,
        extra_args: &[&str],
        script: &str,
        capture_stdout: bool,
    ) -> Result<Vec<u8>, MutatorError> {
        let mut cmd = tokio::process::Command::new("guestfish");
        // The waiter's timeout is the only thing that ends this process, so it
        // must not outlive the future watching it.
        cmd.kill_on_drop(true);
        cmd.arg("-a").arg(&self.disk);
        match &self.mount {
            Some(spec) => {
                cmd.arg("-m").arg(spec);
            }
            None => {
                cmd.arg("-i");
            }
        }
        cmd.args(extra_args);
        cmd.stdin(std::process::Stdio::piped());
        if capture_stdout {
            cmd.stdout(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::process::Stdio::null());
        }
        cmd.stderr(std::process::Stdio::piped());

        let started = std::time::Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| MutatorError::Io(format!("cannot spawn guestfish: {e}")))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| MutatorError::Io("guestfish stdin unavailable".to_string()))?;
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(script.as_bytes())
                .await
                .map_err(|e| MutatorError::Io(format!("cannot write guestfish script: {e}")))?;
        }

        // Wall-clock bound: a stuck appliance used to hang the operation that
        // started it, with nothing in the log and no way for the caller to
        // tell "slow" from "never". `kill_on_drop` makes the appliance die
        // with the future that is waiting for it.
        let timeout = guestfish_timeout();
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| MutatorError::Io(format!("guestfish failed: {e}")))?,
            Err(_) => {
                return Err(MutatorError::Io(format!(
                    "the libguestfs appliance did not finish within {}s — it is stuck starting \
                     (or cannot start on this host); `andler doctor` verifies guestfish and \
                     /dev/kvm, and ANDLERD_GUESTFS_TIMEOUT_SECS raises this bound",
                    timeout.as_secs()
                )))
            }
        };

        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "guestfish session finished"
        );

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr.trim();
            let msg = if reason.is_empty() {
                format!("guestfish exited with {}", output.status)
            } else {
                reason.to_string()
            };
            if msg.contains("not found") || msg.contains("No such file") {
                return Err(MutatorError::NotFound(msg));
            }
            return Err(MutatorError::Io(msg));
        }
        Ok(output.stdout)
    }
}

/// Wraps a guest path in single quotes for the guestfish script parser
/// (which is shell-like: it honors quotes), escaping embedded quotes.
fn quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// Builds the guestfish script for a mutation batch. `WriteFile` content
/// lands in host temp files under `content_dir` and is uploaded with
/// `upload`, keeping binary content out of the script itself.
fn build_script(ops: &[MutatorOp], content_dir: &Path) -> String {
    let mut lines = Vec::with_capacity(ops.len() + 1);
    for (index, op) in ops.iter().enumerate() {
        match op {
            MutatorOp::WriteFile { path, .. } => {
                let tmp = content_dir.join(format!("upload-{index}"));
                lines.push(format!(
                    "upload {} {}",
                    quote(&tmp.to_string_lossy()),
                    quote(path)
                ));
            }
            MutatorOp::UploadFile { path, host_path } => {
                lines.push(format!(
                    "upload {} {}",
                    quote(&host_path.to_string_lossy()),
                    quote(path)
                ));
            }
            MutatorOp::MkdirP { path } => {
                lines.push(format!("mkdir-p {}", quote(path)));
            }
            MutatorOp::CpA { src, dst } => {
                lines.push(format!("cp-a {} {}", quote(src), quote(dst)));
            }
            MutatorOp::Mv { src, dst } => {
                lines.push(format!("mv {} {}", quote(src), quote(dst)));
            }
            MutatorOp::RmRf { path } => {
                lines.push(format!("rm-rf {}", quote(path)));
            }
            MutatorOp::Chmod { path, mode } => {
                lines.push(format!("chmod 0{mode:o} {}", quote(path)));
            }
            MutatorOp::Symlink { target, link } => {
                lines.push(format!("ln-s {} {}", quote(target), quote(link)));
            }
            MutatorOp::RunShell { command } => {
                lines.push(format!("sh {}", quote(command)));
            }
        }
    }
    lines.join("\n")
}

#[async_trait]
impl GuestMutator for GuestfsMutator {
    fn name(&self) -> &'static str {
        "guestfs"
    }

    async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError> {
        if ops.is_empty() {
            return Ok(());
        }
        // WriteFile content is staged into a temp dir; each op's upload
        // references its staging file by index (see build_script).
        let content_dir =
            std::env::temp_dir().join(format!("andler-guestfs-{}-{}", std::process::id(), uuid4()));
        std::fs::create_dir_all(&content_dir)
            .map_err(|e| MutatorError::Io(format!("cannot create staging dir: {e}")))?;

        let write_result = async {
            for (index, op) in ops.iter().enumerate() {
                if let MutatorOp::WriteFile { content, .. } = op {
                    let tmp = content_dir.join(format!("upload-{index}"));
                    std::fs::write(&tmp, content)
                        .map_err(|e| MutatorError::Io(format!("cannot stage upload: {e}")))?;
                }
            }
            let script = build_script(ops, &content_dir);
            self.run_guestfish(&[], &script, false).await?;
            Ok::<(), MutatorError>(())
        }
        .await;

        let _ = std::fs::remove_dir_all(&content_dir);
        write_result
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, MutatorError> {
        let script = format!("download {} -\n", quote(path));
        self.run_guestfish(&["--no-progress"], &script, true).await
    }

    async fn exists(&self, path: &str) -> Result<bool, MutatorError> {
        let script = format!("exists {}\n", quote(path));
        let out = self.run_guestfish(&[], &script, true).await?;
        Ok(String::from_utf8_lossy(&out).trim() == "true")
    }
}

/// Short random hex for staging dir uniqueness (no uuid dependency).
fn uuid4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

#[cfg(test)]
mod timeout_tests {
    use super::{guestfish_timeout, DEFAULT_GUESTFS_TIMEOUT_SECS, MIN_GUESTFS_TIMEOUT_SECS};

    #[test]
    fn the_appliance_bound_is_configurable_and_never_silly() {
        std::env::remove_var("ANDLERD_GUESTFS_TIMEOUT_SECS");
        assert_eq!(guestfish_timeout().as_secs(), DEFAULT_GUESTFS_TIMEOUT_SECS);

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "900");
        assert_eq!(guestfish_timeout().as_secs(), 900);

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "1");
        assert_eq!(guestfish_timeout().as_secs(), MIN_GUESTFS_TIMEOUT_SECS);

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "whenever");
        assert_eq!(guestfish_timeout().as_secs(), DEFAULT_GUESTFS_TIMEOUT_SECS);
        std::env::remove_var("ANDLERD_GUESTFS_TIMEOUT_SECS");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_maps_every_op_to_a_guestfish_command() {
        let ops = vec![
            MutatorOp::WriteFile {
                path: "/etc/foo.conf".to_string(),
                content: b"x".to_vec(),
            },
            MutatorOp::MkdirP {
                path: "/var/lib/foo".to_string(),
            },
            MutatorOp::CpA {
                src: "/a".to_string(),
                dst: "/b".to_string(),
            },
            MutatorOp::Mv {
                src: "/b".to_string(),
                dst: "/c".to_string(),
            },
            MutatorOp::RmRf {
                path: "/old".to_string(),
            },
            MutatorOp::Chmod {
                path: "/etc/foo.conf".to_string(),
                mode: 0o640,
            },
            MutatorOp::Symlink {
                target: "foo.conf".to_string(),
                link: "/etc/foo-link".to_string(),
            },
        ];
        let script = build_script(&ops, Path::new("/tmp/stage"));
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines.len(), ops.len());
        assert_eq!(lines[0], "upload '/tmp/stage/upload-0' '/etc/foo.conf'");
        assert_eq!(lines[1], "mkdir-p '/var/lib/foo'");
        assert_eq!(lines[2], "cp-a '/a' '/b'");
        assert_eq!(lines[3], "mv '/b' '/c'");
        assert_eq!(lines[4], "rm-rf '/old'");
        assert_eq!(lines[5], "chmod 0640 '/etc/foo.conf'");
        assert_eq!(lines[6], "ln-s 'foo.conf' '/etc/foo-link'");
    }

    #[test]
    fn script_quotes_paths_with_quotes_and_spaces() {
        let ops = vec![MutatorOp::MkdirP {
            path: "/tmp/a b'c".to_string(),
        }];
        let script = build_script(&ops, Path::new("/tmp/stage"));
        assert_eq!(script, "mkdir-p '/tmp/a b'\\''c'");
    }

    #[test]
    fn empty_batch_produces_empty_script() {
        assert_eq!(build_script(&[], Path::new("/tmp/stage")), "");
    }
}

#[cfg(test)]
mod conformance_tests {
    use andler_core::guest_mutator::conformance::run_conformance;
    use andler_core::GuestMutator;

    use super::GuestfsMutator;

    #[tokio::test]
    #[ignore = "requires guestfish + qemu-img and a mountable guest image, see docker/e2e/README.md"]
    async fn guestfs_conformance_matches_qga() {
        let disk = std::path::PathBuf::from("/tmp/andler-guestfs-conformance.qcow2");
        let status = tokio::process::Command::new("qemu-img")
            .args(["create", "-f", "raw"])
            .arg(&disk)
            .arg("512M")
            .status()
            .await
            .expect("spawn qemu-img");
        assert!(status.success(), "qemu-img create failed");
        let mkfs = tokio::process::Command::new("mke2fs")
            .args(["-q", "-F", "-t", "ext4"])
            .arg(&disk)
            .status()
            .await
            .expect("spawn mke2fs");
        assert!(mkfs.success(), "mke2fs failed");
        let mutator = GuestfsMutator::with_mount(disk.clone(), "/dev/sda:/".to_string());
        assert_eq!(mutator.name(), "guestfs");
        run_conformance(&mutator).await;
        let _ = std::fs::remove_file(&disk);
    }
}
