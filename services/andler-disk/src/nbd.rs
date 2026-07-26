

use std::path::{Path, PathBuf};

use crate::error::DiskError;


fn mount_dir_base() -> PathBuf {
    andler_core::paths::runtime_dir().join("andler-mounts")
}


/// Builds a `sudo -n <program> ...` command. Connecting/disconnecting nbd devices and
/// mounting/unmounting their partitions needs root (opening `/dev/nbd*` and qemu-nbd's
/// own lock file in `/var/lock` both require it) — but andlerd itself should stay
/// unprivileged rather than running as root wholesale just for this. `-n` (non-interactive)
/// makes sudo fail immediately instead of hanging on a password prompt the daemon has no
/// way to answer; see `describe_sudo_failure` for the resulting error message.
pub(crate) fn privileged_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("sudo");
    cmd.args(["-n", program]);
    cmd
}

/// Turns a failed privileged command's stderr into an actionable error, distinguishing
/// "sudo isn't configured for passwordless use" (the common first-run case) from other
/// failures so the message tells the user exactly what to do.
pub(crate) fn describe_sudo_failure(program: &str, stderr: &str) -> String {
    if stderr.contains("a password is required") || stderr.contains("no tty present") {
        format!(
            "{program} needs root and sudo isn't configured for passwordless use by andlerd. \
             Find the binary's path with `which {program}`, then add a line like this via \
             `sudo visudo`:\n  \
             youruser ALL=(root) NOPASSWD: /usr/bin/{program}\n\
             (replace `youruser` and the path with what `whoami`/`which {program}` show)"
        )
    } else {
        stderr.to_string()
    }
}



pub struct NbdGuard {
    device_path: PathBuf,
}

impl NbdGuard {
    pub fn new(device_path: PathBuf) -> Self {
        NbdGuard { device_path }
    }

    pub fn path(&self) -> &Path {
        &self.device_path
    }
}

impl Drop for NbdGuard {
    fn drop(&mut self) {
        let result = privileged_command("qemu-nbd")
            .args(["--disconnect", &self.device_path.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match result {
            Ok(status) if !status.success() => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    exit_status = %status,
                    "qemu-nbd --disconnect exited with a non-zero status; \
                     /dev/nbd* device may remain connected"
                );
            }
            Err(error) => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    %error,
                    "failed to spawn qemu-nbd --disconnect; \
                     /dev/nbd* device may remain connected"
                );
            }
            Ok(_) => {}
        }
    }
}


pub struct MountGuard {
    mount_point: PathBuf,
}

impl MountGuard {
    pub fn new(mount_point: PathBuf) -> Self {
        MountGuard { mount_point }
    }

    pub fn path(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let result = privileged_command("umount")
            .args(["-l", &self.mount_point.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match result {
            Ok(status) if !status.success() => {
                tracing::warn!(
                    mount_point = %self.mount_point.display(),
                    exit_status = %status,
                    "umount -l exited with a non-zero status"
                );
            }
            Err(error) => {
                tracing::warn!(
                    mount_point = %self.mount_point.display(),
                    %error,
                    "failed to spawn umount -l"
                );
            }
            Ok(_) => {}
        }

        if let Err(error) = std::fs::remove_dir(&self.mount_point) {
            tracing::warn!(
                mount_point = %self.mount_point.display(),
                %error,
                "failed to remove mount point directory"
            );
        }
    }
}



fn scan_nbd_entries(sys_block: &Path) -> Result<Vec<std::fs::DirEntry>, DiskError> {
    let mut entries: Vec<_> = std::fs::read_dir(sys_block)
        .map_err(|e| DiskError::NbdSetupFailed(format!("read /sys/class/block: {e}")))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map_or(false, |name| name.starts_with("nbd"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Tries to load the nbd module (`sudo -n modprobe nbd max_part=8`) when no /dev/nbd*
/// devices exist at all. Best-effort: if the sudoers rule isn't set up, this fails
/// silently here and find_free_nbd_device() falls back to its existing clear error
/// telling the user to run modprobe themselves. Never attempts to unload the module
/// afterwards — an idle nbd module costs nothing, and auto-unload would just be a new
/// source of "module is busy" races between concurrent andler operations.
fn try_autoload_nbd_module() {
    match privileged_command("modprobe")
        .args(["nbd", "max_part=8"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) if output.status.success() => {
            tracing::info!("auto-loaded the nbd kernel module (sudo -n modprobe nbd max_part=8)");
        }
        Ok(output) => {
            tracing::debug!(
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "auto-load of nbd module did not succeed; falling back to manual instructions"
            );
        }
        Err(error) => {
            tracing::debug!(%error, "could not even spawn modprobe for nbd auto-load");
        }
    }
}

pub struct NbdStatus {
    pub loaded: bool,
    pub free_devices: usize,
    pub total_devices: usize,
}

/// Read-only check: is the nbd module loaded, and how many devices are free right now?
/// Never attempts to load the module itself — see `find_free_nbd_device` for that.
pub fn nbd_status() -> NbdStatus {
    let sys_block = Path::new("/sys/class/block");
    let entries = if sys_block.exists() {
        scan_nbd_entries(sys_block).unwrap_or_default()
    } else {
        Vec::new()
    };

    let total_devices = entries.len();
    let free_devices = entries
        .iter()
        .filter(|e| {
            std::fs::read_to_string(e.path().join("size"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .is_some_and(|size| size == 0)
        })
        .count();

    NbdStatus {
        loaded: total_devices > 0,
        free_devices,
        total_devices,
    }
}

pub fn find_free_nbd_device() -> Result<PathBuf, DiskError> {
    let sys_block = Path::new("/sys/class/block");

    if !sys_block.exists() {
        return Err(DiskError::NbdSetupFailed(
            "/sys/class/block does not exist — nbd kernel module not loaded? \
             Run: sudo modprobe nbd"
                .to_string(),
        ));
    }

    let mut entries = scan_nbd_entries(sys_block)?;

    if entries.is_empty() {
        try_autoload_nbd_module();
        entries = scan_nbd_entries(sys_block)?;
    }

    if entries.is_empty() {
        return Err(DiskError::NbdSetupFailed(
            "no /dev/nbd* devices found — the nbd kernel module is probably not loaded. \
             Run: sudo modprobe nbd max_part=8\n\
             To make this persist across reboots, add `nbd` to /etc/modules-load.d/nbd.conf\n\
             (andlerd tried to load it automatically via `sudo -n modprobe`, which only \
             works if you've added a matching NOPASSWD rule — see README.md)"
                .to_string(),
        ));
    }

    let entries_count = entries.len();

    for entry in entries {
        let size_path = entry.path().join("size");
        let size_str = match std::fs::read_to_string(&size_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let size: u64 = match size_str.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        if size == 0 {
            let name = entry.file_name();
            let dev_path = PathBuf::from(format!("/dev/{}", name.to_string_lossy()));
            if dev_path.exists() {
                return Ok(dev_path);
            }
        }
    }

    Err(DiskError::NbdSetupFailed(format!(
        "all {} /dev/nbd* devices are already connected (in use by another qemu-nbd process, \
         or left over from a crash). Free one with: sudo qemu-nbd --disconnect /dev/nbdN \
         — or load more devices: sudo modprobe -r nbd && sudo modprobe nbd max_part=8 max_nbd=32",
        entries_count
    )))
}



pub fn connect_nbd(overlay_path: &Path) -> Result<NbdGuard, DiskError> {
    let device = find_free_nbd_device()?;

    let device_str = device.to_string_lossy().into_owned();
    let overlay_str = overlay_path.to_string_lossy().into_owned();

    let output = privileged_command("qemu-nbd")
        .args([
            "--connect",
            &device_str,
            &overlay_str,
            "--format=qcow2",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run qemu-nbd: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "qemu-nbd --connect failed (exit {}): {}",
            output.status,
            describe_sudo_failure("qemu-nbd", stderr.trim())
        )));
    }

    Ok(NbdGuard::new(device))
}


pub fn wait_for_partitions(nbd_dev: &Path) -> Result<Vec<PathBuf>, DiskError> {
    let dev_name = nbd_dev
        .file_name()
        .ok_or_else(|| DiskError::NbdSetupFailed("invalid nbd device path".to_string()))?
        .to_str()
        .ok_or_else(|| DiskError::NbdSetupFailed("non-utf8 device name".to_string()))?
        .to_string();

    let sys_path = PathBuf::from(format!("/sys/block/{dev_name}"));

    for _ in 0..20 {
        let mut partitions = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&sys_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_str().unwrap_or("");
                if name_str.starts_with(&dev_name) && name_str != dev_name {
                    let part_path = PathBuf::from(format!("/dev/{name_str}"));
                    if part_path.exists() {
                        partitions.push(part_path);
                    }
                }
            }
        }

        if !partitions.is_empty() {
            partitions.sort();
            return Ok(partitions);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(DiskError::NbdSetupFailed(format!(
        "partitions did not appear on {dev_name} within timeout"
    )))
}


pub fn find_root_partition(partitions: &[PathBuf]) -> Result<PathBuf, DiskError> {
    if partitions.is_empty() {
        return Err(DiskError::NbdSetupFailed(
            "no partitions found in image".to_string(),
        ));
    }

    Ok(partitions[0].clone())
}



pub fn unique_mount_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}-{}", std::process::id(), id, ts)
}


pub fn mount_partition(partition: &Path) -> Result<MountGuard, DiskError> {
    let mount_point = mount_dir_base().join(unique_mount_name());

    andler_core::paths::ensure_private_dir_sync(&mount_point).map_err(|e| {
        DiskError::NbdSetupFailed(format!(
            "failed to create mount point {}: {e}",
            mount_point.display()
        ))
    })?;

    let partition_str = partition.to_string_lossy().into_owned();
    let mount_point_str = mount_point.to_string_lossy().into_owned();

    let output = privileged_command("mount")
        .args([
            "-o", "rw",
            &partition_str,
            &mount_point_str,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run mount: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir(&mount_point);
        return Err(DiskError::NbdSetupFailed(format!(
            "mount {} on {} failed: {}",
            partition.display(),
            mount_point.display(),
            describe_sudo_failure("mount", stderr.trim())
        )));
    }

    Ok(MountGuard::new(mount_point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_free_nbd_device_returns_error_when_no_nbd_module() {
        let result = find_free_nbd_device();
        assert!(result.is_err());
    }

    #[test]
    fn unique_mount_name_is_unique() {
        let a = unique_mount_name();
        let b = unique_mount_name();
        assert_ne!(a, b);
    }
}
