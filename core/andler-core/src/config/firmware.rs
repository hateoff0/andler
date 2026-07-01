//! Конфигурация firmware (UEFI/OVMF) инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.4m.fd` +
//! `-drive if=pflash,format=raw,file=<instance>_VARS.fd` (копия шаблона
//! `OVMF_VARS.4m.fd`, создаваемая при первом запуске).
//!
//! `FirmwareConfig` хранит только уже принятые, конкретные пути — автоматический
//! поиск OVMF в системных путях — задача `andler-firmware::detect_matched_pair()`,
//! не этого модуля. Это разделение важно: `andler-core` — чистый домен без
//! зависимости на FS/IO, а детекция — сервис поверх него.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Конфигурация UEFI-загрузки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareConfig {
    /// Путь к системному, общему для всех инстансов, read-only образу
    /// OVMF_CODE. Конкретное значение зависит от дистрибутива —
    /// `andler_firmware::detect_matched_pair()` находит правильный путь
    /// для текущей системы. При создании инстанса через daemon это поле
    /// заполняется авто-определённым или явно переданным значением;
    /// CLI/TOML также может передать свой путь через `--ovmf-code-path`.
    pub ovmf_code_path: PathBuf,
    /// Путь к персональной для инстанса копии OVMF_VARS — здесь хранятся
    /// EFI-переменные конкретной VM (порядок загрузки и т.п.). Файл
    /// создаётся копированием шаблона через `andler_firmware::provision_vars`
    /// при создании инстанса; `andler-qemu::cmdline` только читает готовый путь.
    pub ovmf_vars_path: PathBuf,
}

impl FirmwareConfig {
    /// Конфигурация с явно заданным `ovmf_code_path` — для тестов и мест,
    /// где путь к CODE уже известен (daemon подставляет его из
    /// `andler_firmware::detect_matched_pair()` или из явного CLI-аргумента).
    ///
    /// В продакшн-коде `ovmf_code_path` всегда приходит снаружи;
    /// `reference_default` предназначен для тестов и фикстур, где нужен
    /// полный `FirmwareConfig` без реального детекта OVMF.
    pub fn reference_default(ovmf_vars_path: PathBuf) -> Self {
        FirmwareConfig {
            // Arch/CachyOS путь — для тестовых фикстур; в продакшне
            // это поле всегда заполняется daemon'ом через detect_matched_pair.
            ovmf_code_path: PathBuf::from("/usr/share/edk2/x64/OVMF_CODE.4m.fd"),
            ovmf_vars_path,
        }
    }

    /// Конструктор с явным `ovmf_code_path` — для случаев, когда путь
    /// к CODE уже определён (daemon после `firmware::resolve()`).
    pub fn with_code(ovmf_code_path: PathBuf, ovmf_vars_path: PathBuf) -> Self {
        FirmwareConfig {
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
        assert!(!cfg.ovmf_code_path.as_os_str().is_empty());
        assert_eq!(cfg.ovmf_vars_path, PathBuf::from("/tmp/linux_VARS.fd"));
    }

    #[test]
    fn with_code_sets_both_paths() {
        let code = PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd");
        let vars = PathBuf::from("/home/user/.local/share/andler/instances/abc/VARS.fd");
        let cfg = FirmwareConfig::with_code(code.clone(), vars.clone());
        assert_eq!(cfg.ovmf_code_path, code);
        assert_eq!(cfg.ovmf_vars_path, vars);
    }
}
