use std::path::Path;

use crate::error::DiskError;
use crate::guest_offline::{chroot_exec, prepare_resolv, GuestMount};
use andler_core::config::InstanceKind;
pub use andler_core::package_manager::PackageManager;

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
) -> Result<bool, DiskError> {
    let (query_binary, cmd_args) = pkg_manager.check_installed_command(package);
    let mut argv: Vec<&str> = vec![query_binary];
    argv.extend(cmd_args.iter().copied());
    let output = chroot_exec(mount_point, &argv)?;
    Ok(output.status.success())
}

/// Extra flags for apt inside the unprivileged user namespace: its setuid
/// sandbox is unavailable there, and IPv6-less hosts resolve best over IPv4.
/// These belong to *this* chroot, not to the manager — the in-guest QGA path
/// runs as root with a working sandbox and needs neither, so they must not
/// move into `PackageManager`.
fn userns_apt_flags(manager: PackageManager) -> Vec<&'static str> {
    match manager {
        PackageManager::Apt => vec![
            "-o",
            "APT::Sandbox::User=root",
            "-o",
            "Acquire::ForceIPv4=true",
        ],
        PackageManager::Dnf | PackageManager::Pacman => Vec::new(),
    }
}

pub async fn install_agent_offline(disk_path: &Path, package: &str) -> Result<(), DiskError> {
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
    let mount_guard = GuestMount::mount(disk_path)?;
    prepare_resolv(mount_guard.path())?;
    check_guest_db_access(mount_guard.path())?;

    let pkg_manager = detect_package_manager(mount_guard.path()).ok_or_else(|| {
        DiskError::PackageManagerNotFound {
            mount_point: mount_guard.path().to_path_buf(),
        }
    })?;

    if is_agent_installed(mount_guard.path(), pkg_manager, package)? {
        return Err(DiskError::AgentAlreadyInstalled {
            package: package.to_string(),
        });
    }

    let mut update_argv: Vec<&str> = vec![pkg_manager.binary_name()];
    update_argv.extend(userns_apt_flags(pkg_manager));
    update_argv.extend(pkg_manager.refresh_args());
    let update_output = chroot_exec(mount_guard.path(), &update_argv)?;

    if !update_output.status.success() {
        let stderr = String::from_utf8_lossy(&update_output.stderr);
        if let Ok(resolv) = std::fs::read_to_string(mount_guard.path().join("etc/resolv.conf")) {
            tracing::debug!("package update failed; guest resolv.conf = {:?}", resolv);
        } else if let Ok(meta) =
            std::fs::symlink_metadata(mount_guard.path().join("etc/resolv.conf"))
        {
            tracing::info!(
                "package update failed; guest resolv.conf metadata = {:?}, no readable content",
                meta.file_type()
            );
        } else {
            tracing::info!("package update failed; guest resolv.conf missing");
        }
        return Err(DiskError::NbdSetupFailed(format!(
            "failed to update package indexes in guest (exit {}): {}",
            update_output.status,
            stderr.trim()
        )));
    }

    let cmd_args = pkg_manager.install_args(package);
    let mut install_argv: Vec<&str> = vec![pkg_manager.binary_name()];
    install_argv.extend(userns_apt_flags(pkg_manager));
    install_argv.extend(cmd_args.iter().copied());

    let output = chroot_exec(mount_guard.path(), &install_argv)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "package installation failed (exit {}): {}",
            output.status,
            stderr.trim()
        )));
    }

    enable_systemd_unit(mount_guard.path(), pkg_manager, package)?;

    tracing::info!(package = %package, "package installed successfully in guest filesystem");
    Ok(())
}

/// dpkg verifies its database directory with access(R_OK|W_OK). FUSE
/// access() handling for non-root processes depends on the kernel: on
/// some kernels (observed on cachyos 7.1.8 with libfuse2) access() on a
/// FUSE mount returns EACCES even for the mounting user, and dpkg then
/// aborts with "required read/write access to the dpkg database
/// directory". Detect that up front instead of after a minutes-long apt
/// run, and name the workaround.
fn check_guest_db_access(mount: &Path) -> Result<(), DiskError> {
    let output = chroot_exec(mount, &["/usr/bin/test", "-w", "/var/lib/dpkg"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(DiskError::NbdSetupFailed(
        "the guest's dpkg database is not writable through the FUSE mount: the kernel's FUSE \
         access() handling denies non-root processes in this environment. The zero-root offline \
         path needs a kernel where FUSE access() works for the mounting user, or andlerd running \
         as root (validated in the e2e suite); the smart online path is unaffected"
            .to_string(),
    ))
}

/// deb-systemd-helper cannot create the enable symlink without a running
/// systemd (always true in the offline chroot), so the unit would never
/// start on the next boot. andler links the known unit into
/// multi-user.target.wants itself.
fn enable_systemd_unit(
    mount: &Path,
    pkg_manager: PackageManager,
    package: &str,
) -> Result<(), DiskError> {
    let Some(unit) = KNOWN_PACKAGES
        .iter()
        .chain(ANDROID_PACKAGES.iter())
        .find(|p| p.name == package)
        .and_then(|p| p.systemd_unit)
    else {
        return Ok(());
    };
    let _ = pkg_manager;
    let wants_dir = "/etc/systemd/system/multi-user.target.wants";
    let link = format!("{wants_dir}/{unit}");
    let target = format!("/lib/systemd/system/{unit}");
    let output = chroot_exec(mount, &["ln", "-sf", &target, &link])?;
    if !output.status.success() {
        tracing::warn!(
            package,
            unit,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "could not enable the package systemd unit in the offline chroot; \
             the service may not start on boot"
        );
    }
    Ok(())
}

pub async fn remove_agent_offline(disk_path: &Path, package: &str) -> Result<(), DiskError> {
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
    let mount_guard = GuestMount::mount(disk_path)?;
    check_guest_db_access(mount_guard.path())?;

    let pkg_manager = detect_package_manager(mount_guard.path()).ok_or_else(|| {
        DiskError::PackageManagerNotFound {
            mount_point: mount_guard.path().to_path_buf(),
        }
    })?;

    if !is_agent_installed(mount_guard.path(), pkg_manager, package)? {
        return Err(DiskError::AgentNotInstalled {
            package: package.to_string(),
        });
    }

    let cmd_args = pkg_manager.remove_args(package);
    let mut remove_argv: Vec<&str> = vec![pkg_manager.binary_name()];
    if pkg_manager == PackageManager::Apt {
        remove_argv.extend([
            "-o",
            "APT::Sandbox::User=root",
            "-o",
            "Acquire::ForceIPv4=true",
        ]);
    }
    remove_argv.extend(cmd_args.iter().copied());

    let output = chroot_exec(mount_guard.path(), &remove_argv)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "package removal failed (exit {}): {}",
            output.status,
            stderr.trim()
        )));
    }

    tracing::info!(package = %package, "package removed successfully from guest filesystem");
    Ok(())
}

pub struct GuestPackage {
    pub name: &'static str,
    pub description: &'static str,

    /// Candidate binary paths, checked in order — distributions differ in
    /// where they install the same tool (e.g. `/usr/bin/qemu-ga` on Arch,
    /// `/usr/sbin/qemu-ga` on Debian/Ubuntu).
    pub binary_checks: &'static [&'static str],

    /// systemd unit the package ships. deb-systemd-helper skips the enable
    /// symlink when no systemd is running (always the case in the offline
    /// chroot), so andler creates the multi-user.target.wants link itself
    /// after install; without it the freshly installed service would never
    /// start on the next boot.
    pub systemd_unit: Option<&'static str>,
}

/// Packages every guest can take, whatever the platform: the Android base image
/// is an Arch userland running Waydroid, so the same package manager and the
/// same clipboard/agent packages are there too. Keeping one shared list is what
/// makes `guest list` on an Android VM stop hiding `spice-vdagent` — which
/// `guest apply` installs from `input.clipboard_enabled`.
pub const SHARED_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "spice-vdagent",
        description: "Shared clipboard & copy/paste between host and guest",
        binary_checks: &["/usr/bin/spice-vdagentd", "/usr/sbin/spice-vdagentd"],
        systemd_unit: Some("spice-vdagentd.service"),
    },
    GuestPackage {
        name: "qemu-guest-agent",
        description: "Host-guest communication (guest-exec, freeze/thaw)",
        binary_checks: &["/usr/bin/qemu-ga", "/usr/sbin/qemu-ga"],
        systemd_unit: Some("qemu-guest-agent.service"),
    },
    GuestPackage {
        name: "spice-webdavd",
        description: "Shared folders via SPICE webdav",
        binary_checks: &["/usr/bin/spice-webdavd"],
        systemd_unit: Some("spice-webdavd.service"),
    },
];

/// Linux guests take exactly the shared set.
pub const KNOWN_PACKAGES: &[GuestPackage] = SHARED_PACKAGES;

/// Android-only additions. These are not distribution packages: the translator
/// lands in the Waydroid overlay, so it is staged as files rather than
/// installed, and its "installed" check looks for the staged library.
pub const ANDROID_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "libndk",
        description: "ARM translation (Google NDK, for AMD CPUs)",
        binary_checks: &["var/lib/waydroid/overlay/system/lib/libndk_translation.so"],
        systemd_unit: None,
    },
    GuestPackage {
        name: "libhoudini",
        description: "ARM translation (Intel Houdini)",
        binary_checks: &["var/lib/waydroid/overlay/system/lib/libhoudini.so"],
        systemd_unit: None,
    },
];

/// Everything `guest list` (and the offline check) reports for an instance:
/// the shared packages, then the platform's own.
pub fn packages_for(kind: &InstanceKind) -> Vec<&'static GuestPackage> {
    let shared = SHARED_PACKAGES.iter();
    match kind {
        InstanceKind::LinuxVm { .. } => shared.collect(),
        InstanceKind::AndroidVm { .. } => shared.chain(ANDROID_PACKAGES.iter()).collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Installed,
    NotInstalled,
    Unknown,
}

pub fn check_package_status_offline(mount_point: &Path, binary_checks: &[&str]) -> PackageStatus {
    if binary_checks.iter().any(|binary_check| {
        let full_path = mount_point.join(binary_check.strip_prefix('/').unwrap_or(binary_check));
        full_path.exists()
    }) {
        PackageStatus::Installed
    } else {
        PackageStatus::NotInstalled
    }
}

pub fn check_packages_offline(
    mount_point: &Path,
    kind: &InstanceKind,
) -> Vec<(&'static GuestPackage, PackageStatus)> {
    packages_for(kind)
        .into_iter()
        .map(|pkg| {
            let status = check_package_status_offline(mount_point, pkg.binary_checks);
            (pkg, status)
        })
        .collect()
}

pub fn check_packages_offline_with_disk(
    disk_path: &Path,
    kind: &InstanceKind,
) -> Result<Vec<(&'static GuestPackage, PackageStatus)>, DiskError> {
    let mount_guard = GuestMount::mount(disk_path)?;
    Ok(check_packages_offline(mount_guard.path(), kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn android_kind() -> InstanceKind {
        InstanceKind::AndroidVm {
            android_profile: andler_core::AndroidProfile {
                android_version: andler_core::AndroidVersion::Android13,
                gapps: false,
                microg: false,
                arm_translator: andler_core::ArmTranslator::Libndk,
                boot_mode: andler_core::AndroidBootMode::Android,
                base_image_pin: None,
            },
        }
    }

    fn linux_kind() -> InstanceKind {
        InstanceKind::LinuxVm {
            iso_path: "/tmp/test.iso".into(),
            cdrom_bus: andler_core::CdromBus::Ide,
        }
    }

    #[test]
    fn android_packages_include_the_shared_clipboard_agent() {
        // Regression: `guest list` on an Android VM listed only the ARM
        // translators, so the clipboard agent that `guest apply` installs from
        // `input.clipboard_enabled` looked like it did not exist.
        let names: Vec<&str> = packages_for(&android_kind())
            .iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"spice-vdagent"), "{names:?}");
        assert!(names.contains(&"qemu-guest-agent"), "{names:?}");
        assert!(names.contains(&"libndk"), "{names:?}");
        assert!(names.contains(&"libhoudini"), "{names:?}");
    }

    #[test]
    fn linux_packages_are_the_shared_set_without_translators() {
        let names: Vec<&str> = packages_for(&linux_kind()).iter().map(|p| p.name).collect();
        assert!(names.contains(&"spice-vdagent"), "{names:?}");
        assert!(!names.contains(&"libndk"), "{names:?}");
    }

    #[test]
    fn offline_check_reports_present_and_missing_packages() {
        let dir = std::env::temp_dir().join(format!("andler-test-pkgs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/spice-vdagentd"), b"");

        let statuses = check_packages_offline(&dir, &linux_kind());
        let status = |name: &str| {
            statuses
                .iter()
                .find(|(pkg, _)| pkg.name == name)
                .map(|(_, status)| *status)
        };
        assert_eq!(status("spice-vdagent"), Some(PackageStatus::Installed));
        assert_eq!(
            status("qemu-guest-agent"),
            Some(PackageStatus::NotInstalled)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn package_manager_check_installed_command_runs_the_query_binary() {
        // Regression: the check must run `dpkg`/`rpm` directly — invoking
        // `apt-get dpkg -l …` fails with "unknown command", which made
        // every offline `guest remove` report "package is not installed".
        assert_eq!(
            PackageManager::Apt.check_installed_command("qemu-guest-agent"),
            ("dpkg", vec!["-l", "qemu-guest-agent"])
        );
        assert_eq!(
            PackageManager::Dnf.check_installed_command("qemu-guest-agent"),
            ("rpm", vec!["-q", "qemu-guest-agent"])
        );
        assert_eq!(
            PackageManager::Pacman.check_installed_command("qemu-guest-agent"),
            ("pacman", vec!["-Qi", "qemu-guest-agent"])
        );
    }

    #[test]
    fn check_package_status_offline_returns_installed_when_binary_exists() {
        let dir = std::env::temp_dir().join("andler-test-check-installed");
        let _ = std::fs::create_dir_all(dir.join("usr/bin"));
        let _ = std::fs::write(dir.join("usr/bin/spice-vdagentd"), b"");

        assert_eq!(
            check_package_status_offline(&dir, &["/usr/bin/spice-vdagentd"]),
            PackageStatus::Installed
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_package_status_offline_returns_not_installed_when_binary_missing() {
        let dir = std::env::temp_dir().join("andler-test-check-not-installed");
        let _ = std::fs::create_dir_all(&dir);

        assert_eq!(
            check_package_status_offline(&dir, &["/usr/bin/spice-vdagentd"]),
            PackageStatus::NotInstalled
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_package_status_offline_tries_all_candidate_paths() {
        // Debian-style guests install qemu-ga in /usr/sbin, Arch-style in
        // /usr/bin — the check must accept either location.
        let dir = std::env::temp_dir().join("andler-test-check-sbin");
        let _ = std::fs::create_dir_all(dir.join("usr/sbin"));
        let _ = std::fs::write(dir.join("usr/sbin/qemu-ga"), b"");

        assert_eq!(
            check_package_status_offline(&dir, &["/usr/bin/qemu-ga", "/usr/sbin/qemu-ga"]),
            PackageStatus::Installed
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn known_packages_has_entries() {
        assert!(!KNOWN_PACKAGES.is_empty());
        assert!(KNOWN_PACKAGES.iter().any(|p| p.name == "spice-vdagent"));
    }
}
