//! Конфигурация устройств ввода и интеграции с хостом инстанса.
//!
//! Источник истины — исходная референсная конфигурация (ранее описанная в
//! `scripts/start.sh`, который был удалён после миграции всей логики в Rust):
//! `-device virtio-tablet-pci` (сенсорный/абсолютный ввод — важно для
//! Android-инстансов, где приложения ожидают touch-семантику, а не
//! относительное перемещение мыши), `-display sdl,...,show-cursor=off`,
//! и связка `virtio-serial-pci` + `virtserialport` +
//! `qemu-vdagent,clipboard=on,mouse=on` для буфера обмена между хостом и
//! гостем.

use serde::{Deserialize, Serialize};

/// Тип координат устройства-указателя, которое видит гость.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerMode {
    /// `-device virtio-tablet-pci` — абсолютные координаты (совпадают с
    /// позицией курсора хоста 1:1). Дефолт: курсор хоста и курсор гостя
    /// совпадают в любой момент, без эффекта "мышь застряла на границе
    /// окна", который возникает при relative-режиме без явного grab.
    /// Обязателен для Android-инстансов (Waydroid ожидает touch-события).
    Tablet,
    /// `-device virtio-mouse-pci` — относительные координаты (как у
    /// физической мыши). Нужен для приложений, которые сами захватывают
    /// мышь через relative-движение (например, FPS-игры) — с `Tablet`
    /// такие приложения получают "прыгающий" курсор.
    Mouse,
}

/// Конфигурация ввода и интеграции с хостом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    /// См. [`PointerMode`]. `#[serde(default)]` — старые сериализованные
    /// конфиги без этого поля читаются как `Tablet` (поведение исходной
    /// референсной конфигурации до появления выбора pointer mode), не падают на
    /// десериализации.
    #[serde(default = "default_pointer_mode")]
    pub pointer_mode: PointerMode,
    /// Скрывать курсор хоста в окне отображения (`show-cursor=off`).
    /// Имеет смысл вместе с `pointer_mode = Tablet` — иначе на экране
    /// одновременно курсор хоста и точка тача гостя.
    pub hide_host_cursor: bool,
    /// Включить буфер обмена между хостом и гостем через
    /// `qemu-vdagent`/`virtserialport` (`clipboard=on,mouse=on` в
    /// исходной референсной конфигурации).
    pub clipboard_enabled: bool,
}

fn default_pointer_mode() -> PointerMode {
    PointerMode::Tablet
}

impl InputConfig {
    /// Конфигурация, соответствующая исходной референсной конфигурации:
    /// tablet-режим, курсор хоста скрыт, буфер обмена включён.
    pub fn reference_default() -> Self {
        InputConfig {
            pointer_mode: PointerMode::Tablet,
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
        assert_eq!(cfg.pointer_mode, PointerMode::Tablet);
        assert!(cfg.hide_host_cursor);
        assert!(cfg.clipboard_enabled);
    }

    #[test]
    fn pointer_mode_deserializes_with_default_when_missing() {
        let json = r#"{"hide_host_cursor":true,"clipboard_enabled":true}"#;
        let cfg: InputConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.pointer_mode, PointerMode::Tablet);
    }
}
