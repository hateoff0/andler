use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::error::DiskError;

fn mount_dir_base() -> PathBuf {
    andler_core::paths::runtime_dir().join("andler-mounts")
}

/// Path of the advisory lock file for a given disk image — colocated with the disk
/// itself so it's easy to find/clean, and inherently unique per disk without needing
/// a separate global lock registry.
fn lock_path_for(disk_path: &Path) -> PathBuf {
    let file_name = disk_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "disk".to_string());
    disk_path.with_file_name(format!(".{file_name}.andler-nbd.lock"))
}

/// Takes a blocking, exclusive advisory lock (flock) scoped to this disk image, so
/// two andler processes (or two concurrent operations within the same one) can't
/// both connect nbd devices to the same underlying qcow2 file at once — without
/// this, concurrent offline operations (guest install, boot-mode switch, ...) on the
/// same disk could corrupt it. The lock is released automatically when the returned
/// `File` is dropped (closing the fd releases the flock — standard POSIX behavior),
/// which is why `NbdGuard` holds onto it for its whole lifetime.
fn acquire_disk_lock(disk_path: &Path) -> Result<std::fs::File, DiskError> {
    let lock_path = lock_path_for(disk_path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| {
            DiskError::NbdSetupFailed(format!("failed to open lock file {lock_path:?}: {e}"))
        })?;

    // Blocking: waits for any other andler operation on this same disk image to
    // finish, rather than racing it or failing outright.
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(DiskError::NbdSetupFailed(format!(
            "failed to lock {lock_path:?}: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(file)
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
    /// Held for this guard's entire lifetime; releasing it (via Drop) is what lets
    /// another process's NBD operation on the same disk proceed.
    _disk_lock: std::fs::File,
}

impl NbdGuard {
    pub fn new(device_path: PathBuf, disk_lock: std::fs::File) -> Self {
        NbdGuard {
            device_path,
            _disk_lock: disk_lock,
        }
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
                .is_some_and(|name| name.starts_with("nbd"))
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
pub fn nbd_status() -> Result<NbdStatus, DiskError> {
    let sys_block = Path::new("/sys/class/block");
    let entries = if sys_block.exists() {
        scan_nbd_entries(sys_block)?
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

    Ok(NbdStatus {
        loaded: total_devices > 0,
        free_devices,
        total_devices,
    })
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
    // Locked before even picking a device — otherwise two processes could each
    // grab a *different* free /dev/nbd* and still both end up connected to the
    // same underlying disk image concurrently, which the lock is specifically
    // meant to prevent.
    let disk_lock = acquire_disk_lock(overlay_path)?;

    let device = find_free_nbd_device()?;

    let device_str = device.to_string_lossy().into_owned();
    let overlay_str = overlay_path.to_string_lossy().into_owned();

    let output = privileged_command("qemu-nbd")
        .args(["--connect", &device_str, &overlay_str, "--format=qcow2"])
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

    Ok(NbdGuard::new(device, disk_lock))
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

/// Picks the ext4 root filesystem partition out of an nbd device's partitions.
///
/// `build-disk.sh` always lays the disk out the same way (see its own `Disk layout`
/// doc comment): partition 1 is the vfat ESP (`ANDLER-ESP`, mounted at `/efi`),
/// and partition 2 is the ext4 root (`andler-root`, mounted at `/`) — sized as the
/// rest of the disk, i.e. always the *last* partition, however many there are.
///
/// `wait_for_partitions` returns partitions sorted by device name (`p1`, `p2`, ...),
/// so the root filesystem is always `partitions.last()`, never `partitions[0]` — that
/// would be the ESP itself. Mounting the ESP looks superficially fine (mount succeeds,
/// there's a filesystem there) but every guest-filesystem path built on top of it
/// (boot mode switch/read, ARM translator install, `andler guest install`) then fails
/// to find ordinary root paths like `/etc/systemd/system/...`, since the ESP only
/// holds the UKI/EFI boot files, not the OS.
pub fn find_root_partition(partitions: &[PathBuf]) -> Result<PathBuf, DiskError> {
    partitions
        .last()
        .cloned()
        .ok_or_else(|| DiskError::NbdSetupFailed("no partitions found in image".to_string()))
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

fn bind_host_mounts(mount_point: &Path) {
    // Guest /etc/resolv.conf is often empty or a dangling symlink to
    // /run/systemd/resolve/stub-resolv.conf (systemd-resolved generates it at
    // boot), so package managers inside a chroot cannot resolve mirrors.
    // Write the host nameservers into the guest file instead of bind-mounting:
    // a bind target that is a dangling symlink makes mount(2) fail with ENOENT.
    let guest_resolv = mount_point.join("etc").join("resolv.conf");
    if guest_resolv.is_file() || guest_resolv.symlink_metadata().is_ok() {
        match host_nameservers() {
            Some(contents) => {
                if let Err(e) = write_guest_resolv(mount_point, &contents) {
                    tracing::warn!("failed to write guest /etc/resolv.conf for chroot: {}", e);
                }
            }
            None => tracing::warn!(
                "could not read host nameservers; guest chroot may fail to resolve mirrors"
            ),
        }
    }
    // gpg/pacman and dpkg need device nodes and proc inside the chroot;
    // mirrors of arch-chroot: bind /dev, /proc and /sys.
    for dir in ["/dev", "/proc", "/sys"] {
        let guest_dir = mount_point.join(dir.trim_start_matches('/'));
        if guest_dir.is_dir() {
            bind_host_mount(Path::new(dir), &guest_dir);
        }
    }
    // gpg-agent (used by pacman signature checks) creates sockets under
    // /run; the guest image's /run is normally a boot-time tmpfs and may be
    // unwritable on disk, which makes pacman fail with "GPGME error: Invalid
    // crypto engine". Mirror arch-chroot: mount a fresh tmpfs there.
    let guest_run = mount_point.join("run");
    if guest_run.is_dir() {
        let output = privileged_command("mount")
            .args(["-t", "tmpfs", "tmpfs"])
            .arg(&guest_run)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        match output {
            Ok(out) if out.status.success() => {}
            Ok(out) => tracing::warn!(
                "failed to mount tmpfs on guest /run for chroot: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => tracing::warn!("failed to mount tmpfs on guest /run for chroot: {}", e),
        }
    }
}

fn host_nameservers() -> Option<String> {
    let candidates = ["/etc/resolv.conf", "/run/systemd/resolve/stub-resolv.conf"];
    for path in candidates {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut nameservers: Vec<String> = contents
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                matches!(parts.next(), Some("nameserver"))
                    .then(|| parts.next().map(|ip| format!("nameserver {ip}")))
            })
            .flatten()
            .collect();
        if !nameservers.is_empty() {
            nameservers.dedup();
            return Some(nameservers.join("\n") + "\n");
        }
    }
    None
}

fn write_guest_resolv(mount_point: &Path, contents: &str) -> std::io::Result<()> {
    // The guest filesystem is owned by root and the daemon runs unprivileged,
    // so write through the NOPASSWD-authorized `chroot` instead of direct I/O.
    // Replace a (possibly dangling) symlink first: a regular file is what the
    // chrooted package manager expects.
    let mut child = privileged_command("chroot")
        .arg(mount_point)
        .arg("/bin/sh")
        .args(["-c", "rm -f /etc/resolv.conf && cat > /etc/resolv.conf"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(contents.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "chroot write failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn bind_host_mount(source: &Path, target: &Path) {
    let output = privileged_command("mount")
        .args([
            "--bind",
            &source.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match output {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            tracing::warn!(
                "failed to bind-mount {} into guest chroot: {}",
                source.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            tracing::warn!(
                "failed to bind-mount {} into guest chroot: {e}",
                source.display()
            );
        }
    }
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
        .args(["-o", "rw", &partition_str, &mount_point_str])
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

    bind_host_mounts(&mount_point);

    Ok(MountGuard::new(mount_point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_for_is_colocated_and_hidden() {
        let disk = Path::new("/home/user/.andler/instances/abc/disk.qcow2");
        let lock = lock_path_for(disk);
        assert_eq!(
            lock,
            Path::new("/home/user/.andler/instances/abc/.disk.qcow2.andler-nbd.lock")
        );
    }

    #[test]
    fn acquire_disk_lock_succeeds_on_a_fresh_path() {
        let dir = std::env::temp_dir().join(format!("andler-nbd-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let disk_path = dir.join("disk.qcow2");

        let _lock = acquire_disk_lock(&disk_path).expect("must acquire lock on fresh path");
        assert!(lock_path_for(&disk_path).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn acquire_disk_lock_is_reentrant_within_the_same_process() {
        // flock is per-(process, open file description), not per-process alone, but
        // re-opening and re-locking from the same process on Linux does not deadlock
        // against itself the way a different process would block — this just
        // confirms opening+locking twice in sequence (first dropped, then reacquired)
        // works cleanly, i.e. the lock is properly released when the File is dropped.
        let dir =
            std::env::temp_dir().join(format!("andler-nbd-lock-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let disk_path = dir.join("disk.qcow2");

        {
            let _first = acquire_disk_lock(&disk_path).expect("first lock must succeed");
        }
        let _second =
            acquire_disk_lock(&disk_path).expect("lock must be free after first is dropped");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_free_nbd_device_returns_existing_device_or_explains_absence() {
        // Environment-dependent by nature: on hosts with the nbd module loaded this
        // returns a real device; without it, a helpful error. The old version pinned
        // "no nbd module" and failed on any host that actually has nbd devices.
        match find_free_nbd_device() {
            Ok(dev) => {
                assert!(
                    dev.to_string_lossy().starts_with("/dev/nbd"),
                    "free device must be an /dev/nbd* path, got {dev:?}"
                );
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.to_lowercase().contains("nbd"),
                    "error must explain the nbd situation, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn unique_mount_name_is_unique() {
        let a = unique_mount_name();
        let b = unique_mount_name();
        assert_ne!(a, b);
    }

    #[test]
    fn find_root_partition_picks_the_last_partition_not_the_esp() {
        // Regression test: partitions[0] is /dev/nbd0p1, the ESP created by
        // build-disk.sh — the ext4 root is always the last partition (p2 today,
        // but this must keep working if a layout ever grows a 3rd partition).
        let partitions = vec![PathBuf::from("/dev/nbd0p1"), PathBuf::from("/dev/nbd0p2")];
        assert_eq!(
            find_root_partition(&partitions).unwrap(),
            PathBuf::from("/dev/nbd0p2")
        );
    }

    #[test]
    fn find_root_partition_errors_on_empty_list() {
        assert!(find_root_partition(&[]).is_err());
    }
}
