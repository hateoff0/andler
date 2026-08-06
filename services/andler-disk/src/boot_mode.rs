use std::path::Path;

use andler_core::AndroidBootMode;

use crate::error::DiskError;
use crate::nbd;

fn target_unit_path(mode: AndroidBootMode) -> &'static str {
    match mode {
        AndroidBootMode::Android => "/etc/systemd/system/android.target",
        AndroidBootMode::Linux => "/usr/lib/systemd/system/multi-user.target",
    }
}

fn default_target_link(mount_point: &Path) -> std::path::PathBuf {
    mount_point.join("etc/systemd/system/default.target")
}

pub async fn switch_boot_mode(overlay_path: &Path, mode: AndroidBootMode) -> Result<(), DiskError> {
    if !overlay_path.exists() {
        return Err(DiskError::BackingFileNotFound(overlay_path.to_path_buf()));
    }

    let nbd_guard = nbd::connect_nbd(overlay_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    let target_unit_relative = target_unit_path(mode).trim_start_matches('/');
    let target_unit = mount_guard.path().join(target_unit_relative);
    if !target_unit.exists() {
        return Err(DiskError::FileSystem(format!(
            "{} not found in guest filesystem \u{2014} base image may predate boot mode switching",
            target_unit.display()
        )));
    }

    // /etc/systemd/system is root:root 755 on the guest filesystem (as it is on any
    // normal Linux system) — andlerd itself runs unprivileged, so a raw
    // std::fs::remove_file/symlink here fails with EPERM even though the mount itself
    // is rw. Route the actual mutation through the same privileged chroot pattern
    // guest_tools.rs already uses for package installs: `ln -sfn` both removes the
    // old symlink and creates the new one, so no separate privileged remove is needed.
    let output = nbd::privileged_command("chroot")
        .arg(mount_guard.path())
        .args([
            "ln",
            "-sfn",
            target_unit_path(mode),
            "/etc/systemd/system/default.target",
        ])
        .output()
        .map_err(|e| DiskError::FileSystem(format!("failed to run chroot: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::FileSystem(format!(
            "failed to write default.target symlink (exit {}): {}",
            output.status,
            nbd::describe_sudo_failure("chroot", stderr.trim())
        )));
    }

    tracing::info!(mode = ?mode, path = %overlay_path.display(), "android instance boot mode switched offline");
    Ok(())
}

fn read_boot_mode(mount_point: &Path) -> Result<AndroidBootMode, DiskError> {
    let link = default_target_link(mount_point);
    let target = std::fs::read_link(&link).map_err(|e| {
        DiskError::FileSystem(format!("failed to read default.target symlink: {e}"))
    })?;
    if target.to_string_lossy().ends_with("android.target") {
        Ok(AndroidBootMode::Android)
    } else {
        Ok(AndroidBootMode::Linux)
    }
}

pub fn current_boot_mode(overlay_path: &Path) -> Result<AndroidBootMode, DiskError> {
    if !overlay_path.exists() {
        return Err(DiskError::BackingFileNotFound(overlay_path.to_path_buf()));
    }

    let nbd_guard = nbd::connect_nbd(overlay_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    read_boot_mode(mount_guard.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_boot_mode_recognizes_android_target() {
        let dir = std::env::temp_dir().join("andler-test-boot-mode-android");
        let systemd_dir = dir.join("etc/systemd/system");
        std::fs::create_dir_all(&systemd_dir).unwrap();
        std::os::unix::fs::symlink(
            "/etc/systemd/system/android.target",
            systemd_dir.join("default.target"),
        )
        .unwrap();

        assert_eq!(read_boot_mode(&dir).unwrap(), AndroidBootMode::Android);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_boot_mode_treats_anything_else_as_linux() {
        let dir = std::env::temp_dir().join("andler-test-boot-mode-linux");
        let systemd_dir = dir.join("etc/systemd/system");
        std::fs::create_dir_all(&systemd_dir).unwrap();
        std::os::unix::fs::symlink(
            "/usr/lib/systemd/system/multi-user.target",
            systemd_dir.join("default.target"),
        )
        .unwrap();

        assert_eq!(read_boot_mode(&dir).unwrap(), AndroidBootMode::Linux);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_boot_mode_errors_when_no_default_target_link() {
        let dir = std::env::temp_dir().join("andler-test-boot-mode-missing");
        std::fs::create_dir_all(dir.join("etc/systemd/system")).unwrap();

        assert!(read_boot_mode(&dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
