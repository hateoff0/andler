//! Конфигурация устройств ввода и интеграции с хостом инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-device virtio-tablet-pci` (сенсорный/абсолютный ввод — важно для
//! Android-инстансов, где приложения ожидают touch-семантику, а не
//! относительное перемещение мыши), `-display sdl,...,show-cursor=off`,
//! и связка `virtio-serial-pci` + `virtserialport` +
//! `qemu-vdagent,clipboard=on,mouse=on` для буфера обмена между хостом и
//! гостем.

use serde::{Deserialize, Serialize};

/// Конфигурация ввода и интеграции с хостом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    /// `virtio-tablet-pci` — абсолютное позиционирование (тач/планшет)
    /// вместо относительного движения мыши. `true` соответствует поведению
    /// `start.sh` и обязательно для Android-инстансов (Waydroid ожидает
    /// touch-события, не относительный курсор).
    pub tablet_mode: bool,
    /// Скрывать курсор хоста в окне отображения (`show-cursor=off`).
    /// Имеет смысл вместе с `tablet_mode = true` — иначе на экране
    /// одновременно курсор хоста и точка тача гостя.
    pub hide_host_cursor: bool,
    /// Включить буфер обмена между хостом и гостем через
    /// `qemu-vdagent`/`virtserialport` (`clipboard=on,mouse=on` в
    /// `start.sh`).
    pub clipboard_enabled: bool,
}

impl InputConfig {
    /// Конфигурация, соответствующая `start.sh`: tablet-режим, курсор хоста
    /// скрыт, буфер обмена включён.
    pub fn reference_default() -> Self {
        InputConfig {
            tablet_mode: true,
            hide_host_cursor: true,
            clipboard_enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = InputConfig::reference_default();
        assert!(cfg.tablet_mode);
        assert!(cfg.hide_host_cursor);
        assert!(cfg.clipboard_enabled);
    }
}
