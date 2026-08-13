use async_trait::async_trait;
use std::time::Duration;

use andler_core::{GuestMutator, MutatorError, MutatorOp};

use crate::qmp::{decode_guest_exec_data, QmpClient};

/// Online `GuestMutator` over the QEMU guest agent. Every mutation runs
/// through `guest-exec` with an argv (no shell) or `guest-file-write` for
/// content; zero root on the host. Requires a responsive qemu-guest-agent
/// inside the running guest. The QMP client is owned (QGA chardevs serve
/// one client), and `apply` serializes ops through the internal mutex.
pub struct QgaMutator {
    qga: tokio::sync::Mutex<QmpClient>,
}

impl QgaMutator {
    pub fn new(qga: QmpClient) -> Self {
        QgaMutator {
            qga: tokio::sync::Mutex::new(qga),
        }
    }

    async fn exec_ok(
        &self,
        qga: &mut QmpClient,
        path: &str,
        args: &[&str],
    ) -> Result<Option<String>, MutatorError> {
        let pid = qga
            .guest_exec(path, args)
            .await
            .map_err(|e| MutatorError::Io(e.to_string()))?;

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let status = qga
                .guest_exec_status(pid)
                .await
                .map_err(|e| MutatorError::Io(e.to_string()))?;
            if status.exited {
                let code = status.exitcode.unwrap_or(-1);
                if code != 0 {
                    let stderr = decode_guest_exec_data(status.err_data).unwrap_or_default();
                    return Err(MutatorError::Io(format!(
                        "guest command `{path} {}` exited with {code}: {}",
                        args.join(" "),
                        stderr.trim()
                    )));
                }
                return Ok(decode_guest_exec_data(status.out_data));
            }
            if std::time::Instant::now() > deadline {
                return Err(MutatorError::Io(format!(
                    "guest command `{path} {}` timed out",
                    args.join(" ")
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn apply_one(&self, qga: &mut QmpClient, op: &MutatorOp) -> Result<(), MutatorError> {
        match op {
            MutatorOp::WriteFile { path, content } => qga
                .guest_file_write_bytes(path, content)
                .await
                .map_err(|e| MutatorError::Io(format!("write {path}: {e}"))),
            MutatorOp::MkdirP { path } => {
                self.exec_ok(qga, "mkdir", &["-p", path]).await?;
                Ok(())
            }
            MutatorOp::CpA { src, dst } => {
                self.exec_ok(qga, "cp", &["-a", src, dst]).await?;
                Ok(())
            }
            MutatorOp::Mv { src, dst } => {
                self.exec_ok(qga, "mv", &[src, dst]).await?;
                Ok(())
            }
            MutatorOp::RmRf { path } => {
                self.exec_ok(qga, "rm", &["-rf", path]).await?;
                Ok(())
            }
            MutatorOp::Chmod { path, mode } => {
                self.exec_ok(qga, "chmod", &[&format!("{mode:o}"), path])
                    .await?;
                Ok(())
            }
            MutatorOp::Symlink { target, link } => {
                self.exec_ok(qga, "ln", &["-s", target, link]).await?;
                Ok(())
            }
        }
    }
}

#[async_trait]
impl GuestMutator for QgaMutator {
    fn name(&self) -> &'static str {
        "qga"
    }

    async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError> {
        let mut qga = self.qga.lock().await;
        for op in ops {
            self.apply_one(&mut qga, op).await?;
        }
        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, MutatorError> {
        let mut qga = self.qga.lock().await;
        let out = self.exec_ok(&mut qga, "cat", &[path]).await?;
        out.map(|s| s.into_bytes())
            .ok_or_else(|| MutatorError::Io(format!("cat {path} produced no output")))
    }
}

#[cfg(test)]
mod tests {
    use andler_core::guest_mutator::conformance::run_conformance;
    use andler_core::GuestMutator;

    use super::QgaMutator;
    use crate::qmp::QmpClient;

    #[tokio::test]
    #[ignore = "requires a running QEMU with a responsive qemu-guest-agent, see docker/e2e/README.md"]
    async fn qga_conformance_matches_guestfs() {
        let qga = QmpClient::connect_agent(std::path::Path::new("/tmp/andler/qmp/test.qga.sock"))
            .await
            .expect("connect to test guest agent");
        let mutator = QgaMutator::new(qga);
        assert_eq!(mutator.name(), "qga");
        run_conformance(&mutator).await;
    }
}
