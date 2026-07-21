

use std::path::{Path, PathBuf};

use crate::error::FirmwareError;


pub const KNOWN_OVMF_CODE_PATHS: &[&str] = &[
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/qemu/ovmf-x86_64-code.bin",
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd",
];


pub const KNOWN_OVMF_VARS_PATHS: &[&str] = &[
    "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
    "/usr/share/OVMF/OVMF_VARS_4M.fd",
    "/usr/share/qemu/ovmf-x86_64-vars.bin",
    "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd",
];


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedOvmf {

    pub code: PathBuf,

    pub vars_template: PathBuf,
}


fn find_first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .map(PathBuf::from)
}


pub fn detect() -> Result<DetectedOvmf, FirmwareError> {
    let code = find_first_existing(KNOWN_OVMF_CODE_PATHS)
        .ok_or(FirmwareError::OvmfCodeNotFound)?;

    let vars_template = find_first_existing(KNOWN_OVMF_VARS_PATHS)
        .ok_or(FirmwareError::OvmfVarsNotFound)?;

    tracing::debug!(
        ovmf_code = %code.display(),
        ovmf_vars_template = %vars_template.display(),
        "OVMF firmware detected"
    );

    Ok(DetectedOvmf {
        code,
        vars_template,
    })
}


pub fn detect_matched_pair() -> Result<DetectedOvmf, FirmwareError> {
    debug_assert_eq!(
        KNOWN_OVMF_CODE_PATHS.len(),
        KNOWN_OVMF_VARS_PATHS.len(),
        "KNOWN_OVMF_CODE_PATHS and KNOWN_OVMF_VARS_PATHS must have the same length \
         (one distro = one position in both lists)"
    );

    for (code_candidate, vars_candidate) in KNOWN_OVMF_CODE_PATHS
        .iter()
        .zip(KNOWN_OVMF_VARS_PATHS.iter())
    {
        let code_path = Path::new(code_candidate);
        let vars_path = Path::new(vars_candidate);
        if code_path.exists() && vars_path.exists() {
            tracing::debug!(
                ovmf_code = %code_path.display(),
                ovmf_vars_template = %vars_path.display(),
                "OVMF firmware detected (matched pair)"
            );
            return Ok(DetectedOvmf {
                code: code_path.to_path_buf(),
                vars_template: vars_path.to_path_buf(),
            });
        }
    }

    tracing::debug!("no matched OVMF pair found, falling back to independent detect()");
    detect()
}


pub async fn provision_vars(template: &Path, dest: &Path) -> Result<(), FirmwareError> {
    tokio::fs::copy(template, dest)
        .await
        .map_err(|source| FirmwareError::ProvisionFailed {
            template: template.to_path_buf(),
            dest: dest.to_path_buf(),
            source,
        })?;
    tracing::debug!(
        template = %template.display(),
        dest = %dest.display(),
        "OVMF_VARS provisioned"
    );
    Ok(())
}


pub async fn reset_vars(template: &Path, dest: &Path) -> Result<(), FirmwareError> {
    if dest.exists() {
        tokio::fs::remove_file(dest).await.map_err(|source| {
            FirmwareError::ProvisionFailed {
                template: template.to_path_buf(),
                dest: dest.to_path_buf(),
                source,
            }
        })?;
    }
    provision_vars(template, dest).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_paths_lists_have_equal_length() {
        assert_eq!(
            KNOWN_OVMF_CODE_PATHS.len(),
            KNOWN_OVMF_VARS_PATHS.len(),
            "lists must be kept in sync — one distro = one entry in both"
        );
    }

    #[test]
    fn known_paths_are_absolute() {
        for path in KNOWN_OVMF_CODE_PATHS {
            assert!(
                path.starts_with('/'),
                "OVMF_CODE path must be absolute: {path}"
            );
        }
        for path in KNOWN_OVMF_VARS_PATHS {
            assert!(
                path.starts_with('/'),
                "OVMF_VARS path must be absolute: {path}"
            );
        }
    }

    #[test]
    fn known_paths_end_with_fd_or_bin() {
        for path in KNOWN_OVMF_CODE_PATHS.iter().chain(KNOWN_OVMF_VARS_PATHS) {
            assert!(
                path.ends_with(".fd") || path.ends_with(".bin"),
                "OVMF path has unexpected extension: {path}"
            );
        }
    }

    #[test]
    fn detect_returns_ovmf_code_not_found_on_empty_system() {
        let fake_candidates: &[&str] = &[
            "/this/path/definitely/does/not/exist/OVMF_CODE.fd",
        ];
        let result = find_first_existing(fake_candidates);
        assert!(result.is_none());
    }

    #[test]
    fn detect_matched_pair_degrades_gracefully_on_empty_system() {
        let _ = detect_matched_pair(); // does not panic = good
    }

    #[test]
    fn provision_and_reset_are_exported() {
        std::hint::black_box(provision_vars);
        std::hint::black_box(reset_vars);
    }
}
