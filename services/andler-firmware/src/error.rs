

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {

    #[error(
        "OVMF_CODE not found in any known system path; \
         install edk2-ovmf (Arch/Fedora) or ovmf (Ubuntu/Debian) \
         and re-run, or set --ovmf-code-path explicitly"
    )]
    OvmfCodeNotFound,


    #[error(
        "OVMF_VARS template not found in any known system path; \
         install edk2-ovmf (Arch/Fedora) or ovmf (Ubuntu/Debian) \
         and re-run, or set --ovmf-vars-template explicitly"
    )]
    OvmfVarsNotFound,


    #[error("failed to provision OVMF_VARS from {template} to {dest}: {source}")]
    ProvisionFailed {
        template: PathBuf,
        dest: PathBuf,
        source: std::io::Error,
    },
}
