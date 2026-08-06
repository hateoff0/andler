use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::DiskError;

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

    // SAFETY: statvfs(2) is a POSIX call. The path is NUL-validated via CString above.
    // The output struct is zeroed before use, and the return value is checked.
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

    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

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
        let bytes = available_bytes(&std::env::temp_dir()).expect("statvfs must succeed");
        assert!(
            bytes > 0,
            "a real filesystem should report nonzero free space"
        );
    }

    #[test]
    fn available_bytes_resolves_to_nearest_existing_ancestor() {
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
        let absurd = u64::MAX / 2;
        let err = check_available_space(&path, absurd).unwrap_err();
        assert!(matches!(err, DiskError::InsufficientDiskSpace { .. }));
    }
}
