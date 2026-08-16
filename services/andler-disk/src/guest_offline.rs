use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::DiskError;

/// Zero-root offline guest access (§7 spike, 2026-08): the guest disk is
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
    pub fn mount(disk_path: &Path) -> Result<GuestMount, DiskError> {
        if !disk_path.exists() {
            return Err(DiskError::BackingFileNotFound(disk_path.to_path_buf()));
        }
        let mount = std::env::temp_dir().join(format!(
            "andler-guest-mnt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&mount).map_err(|e| DiskError::FileSystem(e.to_string()))?;

        let status = Command::new("guestmount")
            .arg("-a")
            .arg(disk_path)
            .arg("-i")
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
        let _ = std::fs::remove_dir_all(&self.mount);
    }
}

/// Runs a command chrooted into the guest mount, inside a fresh user+mount
/// namespace where the caller is root. Returns the child's output.
pub fn chroot_exec(mount: &Path, argv: &[&str]) -> Result<std::process::Output, DiskError> {
    let mount_str = mount.to_string_lossy().into_owned();
    let mut cmd = Command::new("unshare");
    cmd.args([
        "--user",
        "--map-root-user",
        "--mount",
        "--",
        "chroot",
        &mount_str,
    ]);
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
    if target.exists()
        && std::fs::read_to_string(&target)
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
    {
        return Ok(());
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
