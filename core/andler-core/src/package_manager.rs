/// Package-manager command specifications — the single source for both the
/// online path (QGA `guest-exec` in the running guest) and the offline path
/// (chroot/qemu-nbd in `andler-disk`). Detection stays in the callers (it
/// touches the filesystem/guest, which `andler-core` must not); the command
/// shapes live here so the two paths can never drift.
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

    /// Refreshes the package index. Both install paths run this first: a guest
    /// that has never synced (a fresh Android image, a cloud image whose mirror
    /// list has moved) otherwise fails the install with "target not found" or a
    /// stale-database error, which reads like a missing package rather than an
    /// outdated index.
    pub fn refresh_args(&self) -> Vec<&'static str> {
        match self {
            PackageManager::Apt => vec!["update"],
            PackageManager::Dnf => vec!["makecache"],
            PackageManager::Pacman => vec!["-Sy"],
        }
    }

    /// A lock file this manager leaves behind when a transaction is killed.
    /// The offline path runs with the guest stopped and the appliance holding
    /// the disk alone, so a lock file there cannot belong to a live
    /// transaction — and pacman's is a plain file rather than a lock the
    /// kernel releases, so one interrupted run (a killed appliance session, a
    /// daemon that died mid-transaction) makes every later transaction fail
    /// with "could not lock database" until it is removed. apt and dnf lock
    /// through the kernel, so they have nothing to clear.
    pub fn stale_lock_path(&self) -> Option<&'static str> {
        match self {
            PackageManager::Apt | PackageManager::Dnf => None,
            PackageManager::Pacman => Some("/var/lib/pacman/db.lck"),
        }
    }

    pub fn remove_args<'a>(&self, package: &'a str) -> Vec<&'a str> {
        match self {
            PackageManager::Apt => vec!["remove", "-y", package],
            PackageManager::Dnf => vec!["remove", "-y", package],
            PackageManager::Pacman => vec!["-R", "--noconfirm", package],
        }
    }

    pub fn check_installed_command<'a>(&self, package: &'a str) -> (&'static str, Vec<&'a str>) {
        match self {
            // The query binary is not the package manager itself: `apt-get`
            // has no `dpkg` subcommand, so the check runs `dpkg`/`rpm`
            // directly inside the chroot.
            PackageManager::Apt => ("dpkg", vec!["-l", package]),
            PackageManager::Dnf => ("rpm", vec!["-q", package]),
            PackageManager::Pacman => ("pacman", vec!["-Qi", package]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_shapes_are_apt_get() {
        let pm = PackageManager::Apt;
        assert_eq!(pm.binary_name(), "apt-get");
        assert_eq!(pm.install_args("vim"), vec!["install", "-y", "vim"]);
        assert_eq!(pm.remove_args("vim"), vec!["remove", "-y", "vim"]);
        assert_eq!(
            pm.check_installed_command("vim"),
            ("dpkg", vec!["-l", "vim"])
        );
    }

    #[test]
    fn dnf_shapes_use_rpm_for_queries() {
        let pm = PackageManager::Dnf;
        assert_eq!(pm.binary_name(), "dnf");
        assert_eq!(pm.install_args("vim"), vec!["install", "-y", "vim"]);
        assert_eq!(
            pm.check_installed_command("vim"),
            ("rpm", vec!["-q", "vim"])
        );
    }

    #[test]
    fn pacman_shapes_are_noconfirm() {
        let pm = PackageManager::Pacman;
        assert_eq!(pm.binary_name(), "pacman");
        assert_eq!(pm.install_args("vim"), vec!["-S", "--noconfirm", "vim"]);
        assert_eq!(pm.remove_args("vim"), vec!["-R", "--noconfirm", "vim"]);
        assert_eq!(
            pm.check_installed_command("vim"),
            ("pacman", vec!["-Qi", "vim"])
        );
    }

    #[test]
    fn only_pacman_leaves_a_lock_file_behind() {
        assert_eq!(
            PackageManager::Pacman.stale_lock_path(),
            Some("/var/lib/pacman/db.lck"),
            "a killed pacman leaves a plain lock file that blocks the next transaction"
        );
        assert_eq!(PackageManager::Apt.stale_lock_path(), None);
        assert_eq!(PackageManager::Dnf.stale_lock_path(), None);
    }

    #[test]
    fn refresh_shapes_match_each_manager() {
        assert_eq!(PackageManager::Apt.refresh_args(), vec!["update"]);
        assert_eq!(PackageManager::Dnf.refresh_args(), vec!["makecache"]);
        assert_eq!(PackageManager::Pacman.refresh_args(), vec!["-Sy"]);
    }
}
