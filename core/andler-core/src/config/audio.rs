//! Конфигурация аудио инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-audiodev pipewire,id=snd0 -device ich9-intel-hda -device hda-output,audiodev=snd0`.

use serde::{Deserialize, Serialize};

/// Backend аудиоподсистемы хоста, используемый QEMU (`-audiodev`).
///
/// Только `Pipewire` соответствует текущему референсному `start.sh`;
/// `Pulseaudio`/`None` добавлены сразу, так как это самый частый источник
/// "не работает звук" на разных дистрибутивах хоста (не у всех PipeWire —
/// дефолтный звуковой сервер), а не гипотетическое расширение на будущее.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioBackend {
    Pipewire,
    Pulseaudio,
    /// Звук отключён (`-audiodev none`-подобное поведение на стороне
    /// `andler-qemu`, либо полное отсутствие audio-устройств — конкретная
    /// реализация решается в `andler-qemu::cmdline`).
    None,
}

/// Конфигурация аудио инстанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub backend: AudioBackend,
}

impl AudioConfig {
    /// Конфигурация, соответствующая `start.sh`: PipeWire.
    pub fn reference_default() -> Self {
        AudioConfig {
            backend: AudioBackend::Pipewire,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = AudioConfig::reference_default();
        assert_eq!(cfg.backend, AudioBackend::Pipewire);
    }
}
