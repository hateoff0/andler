use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::DiskError;

/// Zero-root offline guest access (2026-08): the guest disk is
/// mounted through `guestmount` (libguestfs FUSE — no block mount, no
/// privileges) and commands run inside a user namespace
/// (`unshare --user --map-root-user --mount` + `chroot`). This replaces
/// the old qemu-nbd + mount + `sudo andler-helper chroot-run` kitchen;
/// nothing here needs root or a helper binary.
///
/// Prerequisites (checked by `andler doctor`): `guestmount` on PATH,
/// access to /dev/fuse, and unprivileged user namespaces allowed by the
/// kernel (Debian/Ubuntu: `sysctl kernel.unprivileged_userns_clone=1`).
#[derive(Debug)]
pub struct GuestMount {
    mount: PathBuf,
}

impl GuestMount {
    /// Mounts the guest disk read-write via guestfish inspection (finds
    /// the root filesystem automatically, like the appliance paths do).
    /// One retry after a short pause: a freshly-exited appliance (a
    /// previous mount/drop) can still hold its FUSE socket, and a second
    /// guestmount started immediately then fails with "appliance closed
    /// the connection unexpectedly" — seen reliably in the containerized
    /// e2e (list → install back-to-back).
    pub fn mount(disk_path: &Path) -> Result<GuestMount, DiskError> {
        if !disk_path.exists() {
            return Err(DiskError::BackingFileNotFound(disk_path.to_path_buf()));
        }
        match Self::mount_once(disk_path) {
            Ok(m) => Ok(m),
            Err(first_err) => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                Self::mount_once(disk_path).map_err(|second_err| {
                    tracing::warn!(
                        error = %second_err,
                        "guestmount retry also failed after an appliance race; first error: {first_err}"
                    );
                    second_err
                })
            }
        }
    }

    fn mount_once(disk_path: &Path) -> Result<GuestMount, DiskError> {
        let mount = std::env::temp_dir().join(format!(
            "andler-guest-mnt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&mount).map_err(|e| DiskError::FileSystem(e.to_string()))?;

        // Map guest uid/gid to the mounting user so dpkg/apt can own
        // the files inside the user namespace, and enable POSIX
        // permission checks: without default_permissions the kernel
        // checks access(2) against the mount owner instead of the
        // mapped owners and rejects non-root even as the owner. Two -o
        // flags for uid/gid (the comma form silently drops gid).
        // SAFETY: geteuid/getegid are plain syscall wrappers with no
        // pointer arguments and no failure mode.
        let euid = unsafe { libc::geteuid() };
        // SAFETY: see above.
        let egid = unsafe { libc::getegid() };
        let status = Command::new("guestmount")
            .arg("-a")
            .arg(disk_path)
            .arg("-i")
            .arg("-o")
            .arg(format!("uid={euid}"))
            .arg("-o")
            .arg(format!("gid={egid}"))
            .arg("-o")
            .arg("default_permissions")
            .arg(&mount)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&mount);
                DiskError::NbdSetupFailed(format!(
                    "guestmount is not available (is libguestfs-tools installed?): {e}"
                ))
            })?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            let _ = std::fs::remove_dir_all(&mount);
            return Err(DiskError::NbdSetupFailed(format!(
                "guestmount failed on {}: {} — the image must have a root filesystem \
                 libguestfs can inspect; check that /dev/fuse is accessible",
                disk_path.display(),
                stderr.trim()
            )));
        }

        Ok(GuestMount { mount })
    }

    pub fn path(&self) -> &Path {
        &self.mount
    }
}

impl Drop for GuestMount {
    fn drop(&mut self) {
        // Best-effort: a leftover FUSE mount would pin the image and
        // block later connects. Never silently ignored.
        if let Err(e) = Command::new("guestunmount")
            .arg(&self.mount)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            tracing::warn!(
                mount = %self.mount.display(),
                error = %e,
                "failed to guestunmount; the FUSE mount may be left behind"
            );
        }
        // guestunmount returns once the FUSE mount is gone, but the
        // appliance (libguestfs' own QEMU) exits asynchronously after that
        // and holds the disk image open for a moment — a QEMU spawn right
        // after (maintenance auto-start) then fails with "Failed to get
        // write lock". Wait out the teardown so the file is really free.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(&self.mount);
    }
}

/// Runs a command chrooted into the guest mount, inside a fresh user+mount
/// namespace where the caller is root. Device nodes cannot work through
/// the FUSE mount (open on a device file → EPERM) and devtmpfs cannot be
/// bind-mounted in a user namespace, so the needed nodes are bind-mounted
/// individually from the host (/dev/null, zero, random, urandom, tty,
/// console) plus /proc, /sys and a tmpfs /run — the same recipe the old
/// chroot kitchen used, now without any privileges. Returns the child's
/// output.
pub fn chroot_exec(mount: &Path, argv: &[&str]) -> Result<std::process::Output, DiskError> {
    let mount_str = mount.to_string_lossy().into_owned();
    let script = "mount --bind /dev/null \"$1/dev/null\" 2>/dev/null; \
                  mount --bind /dev/zero \"$1/dev/zero\" 2>/dev/null; \
                  mount --bind /dev/random \"$1/dev/random\" 2>/dev/null; \
                  mount --bind /dev/urandom \"$1/dev/urandom\" 2>/dev/null; \
                  mount --bind /dev/tty \"$1/dev/tty\" 2>/dev/null; \
                  mount --bind /dev/console \"$1/dev/console\" 2>/dev/null; \
                  mount --bind /proc \"$1/proc\" 2>/dev/null; \
                  mount --bind /sys \"$1/sys\" 2>/dev/null; \
                  mount -t tmpfs tmpfs \"$1/run\" 2>/dev/null; \
                  exec chroot \"$1\" \"${@:2}\"";
    let mut cmd = Command::new("unshare");
    cmd.args([
        "--user",
        "--map-root-user",
        "--mount",
        "--",
        "bash",
        "-c",
        script,
    ]);
    cmd.arg("bash").arg(&mount_str);
    cmd.args(argv);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.output().map_err(|e| {
        DiskError::NbdSetupFailed(format!(
            "unshare is not available or unprivileged user namespaces are disabled \
             (kernel.unprivileged_userns_clone=0 on Debian/Ubuntu): {e}"
        ))
    })
}

/// Ensures the guest has a resolv.conf before package-manager network
/// access. The FUSE mount is writable by us, so no helper is involved.
pub fn prepare_resolv(mount: &Path) -> Result<(), DiskError> {
    let target = mount.join("etc/resolv.conf");
    // Cloud images ship resolv.conf as a symlink into /run (a stub that
    // does not exist here) — replace such a link with a plain file.
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&target);
        }
        Ok(_) => {
            let has_content = std::fs::read_to_string(&target)
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false);
            if has_content {
                return Ok(());
            }
        }
        Err(_) => {}
    }
    let host_resolv = std::fs::read_to_string("/etc/resolv.conf")
        .map_err(|e| DiskError::FileSystem(format!("cannot read /etc/resolv.conf: {e}")))?;
    std::fs::write(&target, host_resolv)
        .map_err(|e| DiskError::FileSystem(format!("cannot write {}: {e}", target.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_missing_disk_is_rejected() {
        let err = GuestMount::mount(Path::new("/nonexistent/andler-spike.qcow2")).unwrap_err();
        assert!(matches!(err, DiskError::BackingFileNotFound(_)));
    }

    #[test]
    fn chroot_exec_reports_missing_unshare() {
        // Even with unshare present, a missing binary inside the chroot
        // must surface as a failed child, not a panic.
        if Command::new("unshare")
            .arg("--user")
            .arg("--map-root-user")
            .arg("--mount")
            .arg("--")
            .arg("true")
            .status()
            .is_err()
        {
            return; // environment without unprivileged userns; nothing to assert
        }
        let dir = std::env::temp_dir().join(format!(
            "andler-guest-offline-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let out = chroot_exec(&dir, &["/usr/bin/definitely-not-a-binary"]).unwrap();
        assert!(!out.status.success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
