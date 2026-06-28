//! Конфигурация firmware (UEFI/OVMF) инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.4m.fd` +
//! `-drive if=pflash,format=raw,file=<instance>_VARS.fd` (копия шаблона
//! `OVMF_VARS.4m.fd`, создаваемая при первом запуске).
//!
//! Добавлено в `InstanceConfig` отдельным полем (не частью `DiskConfig`),
//! так как pflash-диски концептуально не диск инстанса, а часть конфигурации
//! виртуальной машины (аналогично BIOS на физическом железе) — у них нет
//! `backing_file`/overlay-семантики и они не участвуют в логике
//! `DiskConfig::overlay`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Конфигурация UEFI-загрузки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareConfig {
    /// Путь к системному, общему для всех инстансов, read-only образу
    /// OVMF_CODE (`/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd` в `start.sh`).
    pub ovmf_code_path: PathBuf,
    /// Путь к персональной для инстанса копии OVMF_VARS — здесь хранятся
    /// EFI-переменные конкретной VM (порядок загрузки и т.п.). Файл создаётся
    /// копированием шаблона при первом запуске инстанса; кто именно копирует
    /// шаблон (andler-daemon при создании инстанса, до первого `spawn`) —
    /// вне ответственности `andler-qemu::cmdline`, которому нужен только
    /// готовый путь.
    pub ovmf_vars_path: PathBuf,
}

impl FirmwareConfig {
    /// Конфигурация, соответствующая `start.sh` (путь к OVMF_CODE — системный
    /// для всех инстансов; `ovmf_vars_path` тут пример — реальный путь
    /// строится `andler-daemon` на основе `InstanceId`).
    pub fn reference_default(ovmf_vars_path: PathBuf) -> Self {
        FirmwareConfig {
            ovmf_code_path: PathBuf::from("/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd"),
            ovmf_vars_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh_code_path() {
        let cfg = FirmwareConfig::reference_default(PathBuf::from("/tmp/linux_VARS.fd"));
        assert_eq!(
            cfg.ovmf_code_path,
            PathBuf::from("/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd")
        );
    }
}
