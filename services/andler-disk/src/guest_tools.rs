

use std::path::Path;

use andler_core::config::InstanceKind;
use crate::error::DiskError;
use crate::nbd;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {

    Apt,

    Dnf,

    Pacman,
}

impl PackageManager {

    pub fn binary_name(&self) -> &'static str {
        match self {
            PackageManager::Apt => "apt-get",
            PackageManager::Dnf => "dnf",
            PackageManager::Pacman => "pacman",
        }
    }


    pub fn install_args<'a>(&self, package: &'a str) -> Vec<&'a str> {
        match self {
            PackageManager::Apt => vec!["install", "-y", package],
            PackageManager::Dnf => vec!["install", "-y", package],
            PackageManager::Pacman => vec!["-S", "--noconfirm", package],
        }
    }


    pub fn remove_args<'a>(&self, package: &'a str) -> Vec<&'a str> {
        match self {
            PackageManager::Apt => vec!["remove", "-y", package],
            PackageManager::Dnf => vec!["remove", "-y", package],
            PackageManager::Pacman => vec!["-R", "--noconfirm", package],
        }
    }


    pub fn check_installed_args<'a>(&self, package: &'a str) -> Vec<&'a str> {
        match self {
            PackageManager::Apt => vec!["dpkg", "-l", package],
            PackageManager::Dnf => vec!["rpm", "-q", package],
            PackageManager::Pacman => vec!["-Qi", package],
        }
    }
}


pub fn detect_package_manager(mount_point: &Path) -> Option<PackageManager> {
    if mount_point.join("usr/bin/apt-get").exists() {
        return Some(PackageManager::Apt);
    }
    if mount_point.join("usr/bin/dnf").exists() {
        return Some(PackageManager::Dnf);
    }
    if mount_point.join("usr/bin/yum").exists() {
        return Some(PackageManager::Dnf);
    }
    if mount_point.join("usr/bin/pacman").exists() {
        return Some(PackageManager::Pacman);
    }
    None
}


pub fn is_agent_installed(
    mount_point: &Path,
    pkg_manager: PackageManager,
    package: &str,
) -> bool {
    let args = pkg_manager.check_installed_args(package);
    let binary = &args[0];
    let cmd_args = &args[1..];

    let output = nbd::privileged_command("chroot")
        .arg(mount_point)
        .arg(binary)
        .args(cmd_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}


pub async fn install_agent_offline(
    disk_path: &Path,
    package: &str,
) -> Result<(), DiskError> {
    let disk_path = disk_path.to_path_buf();
    let package = package.to_string();
    tokio::task::spawn_blocking(move || install_agent_offline_blocking(&disk_path, &package))
        .await
        .unwrap_or_else(|join_err| {
            Err(DiskError::NbdSetupFailed(format!(
                "guest agent install task panicked: {join_err}"
            )))
        })
}

fn install_agent_offline_blocking(disk_path: &Path, package: &str) -> Result<(), DiskError> {
    if !disk_path.exists() {
        return Err(DiskError::BackingFileNotFound(disk_path.to_path_buf()));
    }

    let nbd_guard = nbd::connect_nbd(disk_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    let pkg_manager = detect_package_manager(mount_guard.path()).ok_or_else(|| {
        DiskError::PackageManagerNotFound {
            mount_point: mount_guard.path().to_path_buf(),
        }
    })?;

    if is_agent_installed(mount_guard.path(), pkg_manager, &package) {
        return Err(DiskError::AgentAlreadyInstalled {
            package: package.to_string(),
        });
    }

    let update_args = match pkg_manager {
        PackageManager::Apt => vec!["update"],
        PackageManager::Dnf => vec!["makecache"],
        PackageManager::Pacman => vec!["-Sy"],
    };

    let _ = nbd::privileged_command("chroot")
        .arg(mount_guard.path())
        .arg(pkg_manager.binary_name())
        .args(&update_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();

    let install_args = pkg_manager.install_args(&package);
    let binary = &install_args[0];
    let cmd_args = &install_args[1..];

    let output = nbd::privileged_command("chroot")
        .arg(mount_guard.path())
        .arg(binary)
        .args(cmd_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run chroot: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "package installation failed (exit {}): {}",
            output.status,
            nbd::describe_sudo_failure("chroot", stderr.trim())
        )));
    }

    tracing::info!(package = %package, "package installed successfully in guest filesystem");
    Ok(())
}


pub async fn remove_agent_offline(
    disk_path: &Path,
    package: &str,
) -> Result<(), DiskError> {
    let disk_path = disk_path.to_path_buf();
    let package = package.to_string();
    tokio::task::spawn_blocking(move || remove_agent_offline_blocking(&disk_path, &package))
        .await
        .unwrap_or_else(|join_err| {
            Err(DiskError::NbdSetupFailed(format!(
                "guest agent remove task panicked: {join_err}"
            )))
        })
}

fn remove_agent_offline_blocking(disk_path: &Path, package: &str) -> Result<(), DiskError> {
    if !disk_path.exists() {
        return Err(DiskError::BackingFileNotFound(disk_path.to_path_buf()));
    }

    let nbd_guard = nbd::connect_nbd(disk_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    let pkg_manager = detect_package_manager(mount_guard.path()).ok_or_else(|| {
        DiskError::PackageManagerNotFound {
            mount_point: mount_guard.path().to_path_buf(),
        }
    })?;

    if !is_agent_installed(mount_guard.path(), pkg_manager, &package) {
        return Err(DiskError::AgentNotInstalled {
            package: package.to_string(),
        });
    }

    let remove_args = pkg_manager.remove_args(&package);
    let binary = &remove_args[0];
    let cmd_args = &remove_args[1..];

    let output = nbd::privileged_command("chroot")
        .arg(mount_guard.path())
        .arg(binary)
        .args(cmd_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run chroot: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "package removal failed (exit {}): {}",
            output.status,
            nbd::describe_sudo_failure("chroot", stderr.trim())
        )));
    }

    tracing::info!(package = %package, "package removed successfully from guest filesystem");
    Ok(())
}



pub struct GuestPackage {
    pub name: &'static str,
    pub description: &'static str,

    pub binary_check: &'static str,
}


pub const KNOWN_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "spice-vdagent",
        description: "Shared clipboard & copy/paste between host and guest",
        binary_check: "/usr/bin/spice-vdagentd",
    },
    GuestPackage {
        name: "qemu-guest-agent",
        description: "Host-guest communication (guest-exec, freeze/thaw)",
        binary_check: "/usr/bin/qemu-ga",
    },
    GuestPackage {
        name: "spice-webdavd",
        description: "Shared folders via SPICE webdav",
        binary_check: "/usr/bin/spice-webdavd",
    },
];

pub const ANDROID_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "libndk",
        description: "ARM translation (Google NDK, for AMD CPUs)",
        binary_check: "var/lib/waydroid/overlay/system/lib/libndk_translation.so",
    },
    GuestPackage {
        name: "libhoudini",
        description: "ARM translation (Intel Houdini)",
        binary_check: "var/lib/waydroid/overlay/system/lib/libhoudini.so",
    },
];


pub fn available_packages(kind: &InstanceKind) -> &'static [GuestPackage] {
    match kind {
        InstanceKind::AndroidVm { .. } => ANDROID_PACKAGES,
        InstanceKind::LinuxVm { .. } => KNOWN_PACKAGES,
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Installed,
    NotInstalled,
    Unknown,
}


pub fn check_package_status_offline(mount_point: &Path, binary_check: &str) -> PackageStatus {
    let full_path = mount_point.join(binary_check.strip_prefix('/').unwrap_or(binary_check));
    if full_path.exists() {
        PackageStatus::Installed
    } else {
        PackageStatus::NotInstalled
    }
}


pub fn check_all_packages_offline(mount_point: &Path) -> Vec<(&'static GuestPackage, PackageStatus)> {
    KNOWN_PACKAGES
        .iter()
        .map(|pkg| {
            let status = check_package_status_offline(mount_point, pkg.binary_check);
            (pkg, status)
        })
        .collect()
}


pub fn check_all_packages_offline_with_disk(
    disk_path: &Path,
) -> Result<Vec<(&'static GuestPackage, PackageStatus)>, DiskError> {
    let nbd_guard = nbd::connect_nbd(disk_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root)?;

    let results = check_all_packages_offline(mount_guard.path());
    Ok(results)
}


pub fn check_android_packages_offline_with_disk(
    disk_path: &Path,
) -> Result<Vec<(&'static GuestPackage, PackageStatus)>, DiskError> {
    let nbd_guard = nbd::connect_nbd(disk_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root)?;

    let results = ANDROID_PACKAGES
        .iter()
        .map(|pkg| {
            let status = check_package_status_offline(mount_guard.path(), pkg.binary_check);
            (pkg, status)
        })
        .collect();
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_package_manager_returns_apt() {
        let dir = std::env::temp_dir().join("andler-test-detect-apt");
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/apt-get"), b"");

        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Apt));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_package_manager_returns_dnf() {
        let dir = std::env::temp_dir().join("andler-test-detect-dnf");
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/dnf"), b"");

        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Dnf));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_package_manager_returns_pacman() {
        let dir = std::env::temp_dir().join("andler-test-detect-pacman");
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/pacman"), b"");

        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Pacman));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_package_manager_returns_none_for_empty_dir() {
        let dir = std::env::temp_dir().join("andler-test-detect-none");
        let _ = std::fs::create_dir_all(&dir);

        assert_eq!(detect_package_manager(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_manager_install_args() {
        assert_eq!(
            PackageManager::Apt.install_args("spice-vdagent"),
            vec!["install", "-y", "spice-vdagent"]
        );
        assert_eq!(
            PackageManager::Dnf.install_args("spice-vdagent"),
            vec!["install", "-y", "spice-vdagent"]
        );
        assert_eq!(
            PackageManager::Pacman.install_args("spice-vdagent"),
            vec!["-S", "--noconfirm", "spice-vdagent"]
        );
    }

    #[test]
    fn package_manager_remove_args() {
        assert_eq!(
            PackageManager::Apt.remove_args("spice-vdagent"),
            vec!["remove", "-y", "spice-vdagent"]
        );
        assert_eq!(
            PackageManager::Dnf.remove_args("spice-vdagent"),
            vec!["remove", "-y", "spice-vdagent"]
        );
        assert_eq!(
            PackageManager::Pacman.remove_args("spice-vdagent"),
            vec!["-R", "--noconfirm", "spice-vdagent"]
        );
    }

    #[test]
    fn check_package_status_offline_returns_installed_when_binary_exists() {
        let dir = std::env::temp_dir().join("andler-test-check-installed");
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/spice-vdagentd"), b"");

        assert_eq!(
            check_package_status_offline(&dir, "/usr/bin/spice-vdagentd"),
            PackageStatus::Installed
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_package_status_offline_returns_not_installed_when_binary_missing() {
        let dir = std::env::temp_dir().join("andler-test-check-not-installed");
        let _ = std::fs::create_dir_all(&dir);

        assert_eq!(
            check_package_status_offline(&dir, "/usr/bin/spice-vdagentd"),
            PackageStatus::NotInstalled
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn known_packages_has_entries() {
        assert!(!KNOWN_PACKAGES.is_empty());
        assert!(KNOWN_PACKAGES.iter().any(|p| p.name == "spice-vdagent"));
    }
}
