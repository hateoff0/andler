use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use tokio::io::unix::AsyncFd;

#[derive(Debug)]
pub struct PidFd {
    pid: u32,
    inner: AsyncFd<OwnedFd>,
}

impl PidFd {
    pub fn open(pid: u32) -> io::Result<PidFd> {
        // SAFETY: pidfd_open allocates a fresh file descriptor that references
        // the process with the given pid; it does not dereference any pointer
        // and cannot corrupt memory. The returned fd is wrapped in OwnedFd
        // immediately, so it is always closed exactly once.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = fd as RawFd;
        // SAFETY: pidfd_open returned a fresh, owned descriptor above.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        set_nonblocking(&owned)?;
        let inner = AsyncFd::new(owned)?;
        Ok(PidFd { pid, inner })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub async fn wait_for_exit(&self) -> io::Result<ExitStatus> {
        let _ready = self.inner.readable().await?;
        Ok(Self::reap(self.pid))
    }

    pub fn has_exited(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.inner.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll on a valid fd; the struct is initialized above.
        let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
        ready > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP)) != 0
    }

    fn reap(pid: u32) -> ExitStatus {
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: pid is an already-exited process we own; waitpid with
            // WNOHANG reaps its zombie. ECHILD means the process was never a
            // child of ours (only possible with an adopted pidfd), in which
            // case there is nothing to reap.
            let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if result == pid as libc::pid_t {
                return ExitStatus::from_raw(status);
            }
            let errno = io::Error::last_os_error();
            match errno.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {
                    std::thread::yield_now();
                }
                io::ErrorKind::NotFound => {
                    return ExitStatus::default();
                }
                _ => {
                    tracing::warn!(pid, error = %errno, "waitpid after pidfd exit failed");
                    return ExitStatus::default();
                }
            }
        }
    }
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fcntl on a valid fd; cannot corrupt memory.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl on a valid fd; setting the O_NONBLOCK flag.
    let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn spawn_exiting_child() -> (std::process::Child, u32) {
        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh must be present in the test environment");
        let pid = child.id();
        (child, pid)
    }

    fn spawn_long_lived_child() -> (std::process::Child, u32) {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh must be present in the test environment");
        let pid = child.id();
        (child, pid)
    }

    #[tokio::test]
    async fn open_and_pid_roundtrip() {
        let (mut child, pid) = spawn_exiting_child();
        let pidfd = PidFd::open(pid).unwrap();
        assert_eq!(pidfd.pid(), pid);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    async fn wait_for_exit_reports_dead_process() {
        let (mut child, pid) = spawn_exiting_child();
        let pidfd = PidFd::open(pid).unwrap();
        let status = pidfd.wait_for_exit().await.unwrap();
        assert!(status.success());
        let _ = child.wait();
    }

    #[tokio::test]
    async fn has_exited_transitions_after_death() {
        let (mut child, pid) = spawn_exiting_child();
        let pidfd = PidFd::open(pid).unwrap();
        assert!(!pidfd.has_exited());
        let _ = pidfd.wait_for_exit().await;
        assert!(pidfd.has_exited());
        let _ = child.wait();
    }

    #[tokio::test]
    async fn wait_for_exit_blocks_until_kill() {
        let (mut child, pid) = spawn_long_lived_child();
        let pidfd = PidFd::open(pid).unwrap();
        assert!(!pidfd.has_exited());

        let wait_task = tokio::spawn(async move {
            let pfd = PidFd::open(pid).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), pfd.wait_for_exit())
                .await
                .expect("process must exit within timeout")
                .expect("wait must succeed")
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        child.kill().unwrap();
        let _ = child.wait();

        wait_task.await.unwrap();
        let _ = pidfd;
        assert!(pidfd.has_exited());
    }
}
