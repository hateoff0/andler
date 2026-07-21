

use std::path::PathBuf;

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareConfig {

    #[serde(default = "default_true")]
    pub enable_uefi: bool,

    pub ovmf_code_path: PathBuf,

    pub ovmf_vars_path: PathBuf,
}

fn default_true() -> bool {
    true
}

impl FirmwareConfig {

    pub fn reference_default(ovmf_vars_path: PathBuf) -> Self {
        FirmwareConfig {
            enable_uefi: true,
            ovmf_code_path: PathBuf::from("/usr/share/edk2/x64/OVMF_CODE.4m.fd"),
            ovmf_vars_path,
        }
    }

    pub fn with_code(ovmf_code_path: PathBuf, ovmf_vars_path: PathBuf) -> Self {
        FirmwareConfig {
            enable_uefi: true,
            ovmf_code_path,
            ovmf_vars_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_has_expected_structure() {
        let cfg = FirmwareConfig::reference_default(PathBuf::from("/tmp/linux_VARS.fd"));
        assert!(cfg.enable_uefi);
        assert!(!cfg.ovmf_code_path.as_os_str().is_empty());
        assert_eq!(cfg.ovmf_vars_path, PathBuf::from("/tmp/linux_VARS.fd"));
    }

    #[test]
    fn with_code_sets_both_paths() {
        let code = PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd");
        let vars = PathBuf::from("/home/user/.andler/instances/abc/VARS.fd");
        let cfg = FirmwareConfig::with_code(code.clone(), vars.clone());
        assert!(cfg.enable_uefi);
        assert_eq!(cfg.ovmf_code_path, code);
        assert_eq!(cfg.ovmf_vars_path, vars);
    }
}
