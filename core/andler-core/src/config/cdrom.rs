

use std::path::Path;

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CdromBus {

    VirtioScsi,

    Ide,
}

impl CdromBus {

    pub fn description(&self) -> &'static str {
        match self {
            CdromBus::VirtioScsi => {
                "Faster mounting and reading of installation files. Suitable if \
                 the installation media is a modern Linux distribution (initrd \
                 almost always supports virtio-scsi). If the installation does not boot — \
                 switch to ide."
            }
            CdromBus::Ide => {
                "Slower, but works without virtio drivers in boot environment — \
                 compatible with any guest (Windows, unknown ISO, old OS)."
            }
        }
    }


    const KNOWN_VIRTIO_FRIENDLY_DISTROS: &'static [&'static str] = &[
        "cachyos",
        "ubuntu",
        "kubuntu",
        "xubuntu",
        "lubuntu",
        "fedora",
        "debian",
        "arch",
        "manjaro",
        "mint",
        "pop-os",
        "pop_os",
        "opensuse",
        "endeavouros",
        "garuda",
        "kali",
        "nixos",
    ];


    pub fn recommended_for_iso_filename(iso_path: &Path) -> Self {
        let Some(file_name) = iso_path.file_name().and_then(|n| n.to_str()) else {
            return CdromBus::Ide;
        };
        let lower = file_name.to_lowercase();

        if Self::KNOWN_VIRTIO_FRIENDLY_DISTROS
            .iter()
            .any(|distro| lower.contains(distro))
        {
            CdromBus::VirtioScsi
        } else {
            CdromBus::Ide
        }
    }
}

impl Default for CdromBus {

    fn default() -> Self {
        CdromBus::Ide
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recommends_virtio_scsi_for_known_distros() {
        for name in [
            "cachyos-desktop-linux-260426.iso",
            "ubuntu-24.04-desktop-amd64.iso",
            "Fedora-Workstation-Live-x86_64-41.iso",
            "archlinux-2026.06.01-x86_64.iso",
            "manjaro-kde-26.0-minimal-linux616.iso",
        ] {
            assert_eq!(
                CdromBus::recommended_for_iso_filename(&PathBuf::from(name)),
                CdromBus::VirtioScsi,
                "expected VirtioScsi for {name}"
            );
        }
    }

    #[test]
    fn recommends_ide_for_unknown_or_windows_iso() {
        for name in [
            "Win11_24H2_English_x64.iso",
            "random-image.iso",
            "custom-build.iso",
        ] {
            assert_eq!(
                CdromBus::recommended_for_iso_filename(&PathBuf::from(name)),
                CdromBus::Ide,
                "expected Ide for {name}"
            );
        }
    }

    #[test]
    fn recommends_ide_when_path_has_no_filename() {
        assert_eq!(
            CdromBus::recommended_for_iso_filename(&PathBuf::from("/")),
            CdromBus::Ide
        );
    }

    #[test]
    fn default_is_ide_not_virtio() {
        assert_eq!(CdromBus::default(), CdromBus::Ide);
    }
}
