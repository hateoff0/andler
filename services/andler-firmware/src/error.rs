//! Ошибки крейта `andler-firmware`.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    /// Ни один из известных системных путей OVMF_CODE не найден.
    /// Пользователю нужно установить пакет OVMF/EDK2:
    ///   Arch/Manjaro: `sudo pacman -S edk2-ovmf`
    ///   Ubuntu/Debian: `sudo apt install ovmf`
    ///   Fedora:        `sudo dnf install edk2-ovmf`
    #[error(
        "OVMF_CODE not found in any known system path; \
         install edk2-ovmf (Arch/Fedora) or ovmf (Ubuntu/Debian) \
         and re-run, or set --ovmf-code-path explicitly"
    )]
    OvmfCodeNotFound,

    /// Ни один из известных системных путей OVMF_VARS-шаблона не найден.
    /// Обычно этот пакет устанавливается вместе с OVMF_CODE, но на
    /// некоторых дистрибутивах файлы могут лежать не там, где ожидается.
    #[error(
        "OVMF_VARS template not found in any known system path; \
         install edk2-ovmf (Arch/Fedora) or ovmf (Ubuntu/Debian) \
         and re-run, or set --ovmf-vars-template explicitly"
    )]
    OvmfVarsNotFound,

    /// Не удалось скопировать шаблон OVMF_VARS в персональный путь
    /// инстанса. Такое происходит при нехватке прав или места на диске.
    #[error("failed to provision OVMF_VARS from {template} to {dest}: {source}")]
    ProvisionFailed {
        template: PathBuf,
        dest: PathBuf,
        source: std::io::Error,
    },
}
