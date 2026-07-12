//! Free-disk-space pre-check — see ROADMAP.md, "Core: add disk space
//! pre-check before snapshot operations".
//!
//! QEMU's internal qcow2 snapshots (`snapshot-save`) grow the *same*
//! disk file rather than copying it elsewhere, but a filesystem that's
//! already nearly full can still run out of room partway through the
//! write — QEMU (or the kernel) then reports a raw I/O error mid-job,
//! which is a worse experience than refusing up front with a clear
//! reason. There's no way to know a snapshot's exact size before taking
//! it (it depends on how much guest RAM/disk state actually changed
//! since the last one), so `required_bytes` here is deliberately a
//! conservative estimate, not a guarantee — see the call site in
//! `daemon/src/daemon/snapshot_ops.rs` for how it's derived.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::DiskError;

/// Bytes free on the filesystem containing `path`. `path` itself doesn't
/// need to exist yet — its nearest existing ancestor directory is used,
/// same resolution `df`/`statvfs` callers conventionally rely on for a
/// not-yet-created file.
fn available_bytes(path: &Path) -> Result<u64, DiskError> {
    let mut probe: &Path = path;
    loop {
        if probe.exists() {
            break;
        }
        probe = probe.parent().ok_or_else(|| DiskError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no existing ancestor directory found to check free space against",
            ),
        })?;
    }

    let c_path = CString::new(probe.as_os_str().as_bytes()).map_err(|_| DiskError::Io {
        path: probe.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an interior NUL byte, cannot pass to statvfs(2)",
        ),
    })?;

    // SAFETY: `stat` is a plain-old-data struct fully initialized by a
    // successful `statvfs` call before we read any field from it; on
    // failure we never touch `stat` and return the OS error instead.
    // `c_path` stays alive for the duration of the call (it's a local
    // binding, not dropped until this function returns).
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return Err(DiskError::Io {
                path: probe.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }
        stat
    };

    // `f_bavail` — blocks available to an unprivileged user (not
    // `f_bfree`, which includes root-reserved blocks the daemon may not
    // actually be able to use, e.g. ext4's default 5% reservation).
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// Fails with [`DiskError::InsufficientDiskSpace`] if fewer than
/// `required_bytes` are free on the filesystem containing `path` —
/// before the caller starts an operation that could otherwise run out of
/// space partway through.
pub fn check_available_space(path: &Path, required_bytes: u64) -> Result<(), DiskError> {
    let available = available_bytes(path)?;
    if available < required_bytes {
        return Err(DiskError::InsufficientDiskSpace {
            path: PathBuf::from(path),
            required_bytes,
            available_bytes: available,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_bytes_on_temp_dir_is_nonzero() {
        // std::env::temp_dir() always exists on any machine this runs on
        // — a real, deterministic sanity check that statvfs itself works,
        // without needing to fabricate a filesystem with known free space.
        let bytes = available_bytes(&std::env::temp_dir()).expect("statvfs must succeed");
        assert!(bytes > 0, "a real filesystem should report nonzero free space");
    }

    #[test]
    fn available_bytes_resolves_to_nearest_existing_ancestor() {
        // The file itself doesn't exist, but its parent (temp_dir) does —
        // must resolve to that, not error out.
        let path = std::env::temp_dir().join("andler-test-diskspace-nonexistent-file.qcow2");
        assert!(!path.exists());
        let bytes = available_bytes(&path).expect("must resolve via nearest existing ancestor");
        assert!(bytes > 0);
    }

    #[test]
    fn check_available_space_passes_for_a_tiny_requirement() {
        let path = std::env::temp_dir().join("andler-test-diskspace-check.qcow2");
        assert!(check_available_space(&path, 1).is_ok());
    }

    #[test]
    fn check_available_space_fails_for_an_absurd_requirement() {
        let path = std::env::temp_dir().join("andler-test-diskspace-check.qcow2");
        // No real filesystem has an exabyte free — this is deterministic
        // without needing to fill up a real disk to test the failure path.
        let absurd = u64::MAX / 2;
        let err = check_available_space(&path, absurd).unwrap_err();
        assert!(matches!(err, DiskError::InsufficientDiskSpace { .. }));
    }
}
