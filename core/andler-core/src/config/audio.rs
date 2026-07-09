//! Конфигурация аудио инстанса.
//!
//! Источник истины — исходная референсная конфигурация (ранее описанная в
//! `scripts/start.sh`, который был удалён после миграции всей логики в Rust):
//! `-audiodev pipewire,id=snd0 -device ich9-intel-hda -device hda-output,audiodev=snd0`.

use serde::{Deserialize, Serialize};

/// Backend аудиоподсистемы хоста, используемый QEMU (`-audiodev`).
///
/// Только `Pipewire` соответствует исходной референсной конфигурации;
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

/// Звуковое PCI-устройство, которое видит гость — независимая ось от
/// [`AudioBackend`] (backend хоста). См. PLAN.md, раздел "Настройки
/// звука": оба устройства способны работать с любым backend'ом хоста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioDevice {
    /// `-device virtio-sound-pci` — современный paravirtualized путь,
    /// ниже латентность, чище звук. Требует гостевое ядро ≥5.13,
    /// собранное с `CONFIG_SND_VIRTIO` — у части дистрибутивов гостя эта
    /// опция выключена по умолчанию, гарантии нет. Дефолт для новых VM
    /// (ANDLER проверяет работоспособность после первого запуска и
    /// предлагает переключиться на `Ich9Hda`, если звук не подхватился —
    /// не делает тихий даунгрейд).
    VirtioSound,
    /// `-device ich9-intel-hda` — заметно выше латентность, но работает с
    /// любым стандартным ALSA/HDA-драйвером без пересборки ядра гостя.
    /// Максимальная совместимость, fallback-вариант. Это то, что делала
    /// исходная референсная конфигурация до появления `virtio-sound-pci` как дефолта.
    Ich9Hda,
}

/// Конфигурация аудио инстанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub backend: AudioBackend,
    /// PCI-устройство, которое видит гость. `#[serde(default)]` —
    /// инстансы, созданные до появления этого поля, не должны сломаться
    /// при чтении со старым сериализованным конфигом (см. прецедент
    /// `DiskConfig::compact_on_shutdown`); дефолт для них — `VirtioSound`,
    /// как и для новых.
    #[serde(default = "default_audio_device")]
    pub device: AudioDevice,
}

fn default_audio_device() -> AudioDevice {
    AudioDevice::VirtioSound
}

impl AudioConfig {
    /// **Отклоняется от буквальной исходной референсной конфигурации** (там всегда
    /// `ich9-intel-hda`): дефолтное устройство — `virtio-sound-pci`,
    /// т.к. оно объективно быстрее и чище там, где гостевое ядро его
    /// поддерживает — см. PLAN.md, раздел "Настройки звука" за полным
    /// обоснованием (аналогично отклонению дефолтного размера диска
    /// 40→256 GiB от исходной референсной конфигурации в `DiskConfig::reference_default`).
    /// Backend хоста (`AudioBackend`) по-прежнему соответствует
    /// исходной референсной конфигурации: PipeWire.
    pub fn reference_default() -> Self {
        AudioConfig {
            backend: AudioBackend::Pipewire,
            device: AudioDevice::VirtioSound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_backend_matches_start_sh() {
        let cfg = AudioConfig::reference_default();
        assert_eq!(cfg.backend, AudioBackend::Pipewire);
    }

    #[test]
    fn reference_default_device_is_virtio_sound_not_start_sh() {
        // Умышленное отклонение от исходной референсной конфигурации (там ich9-intel-hda) — см.
        // doc-комментарий reference_default(). Не откатывайте это на
        // ich9-intel-hda без пересмотра PLAN.md, раздел "Настройки звука".
        let cfg = AudioConfig::reference_default();
        assert_eq!(cfg.device, AudioDevice::VirtioSound);
    }

    #[test]
    fn device_field_deserializes_with_default_when_missing() {
        // Старые сериализованные конфиги (до появления поля `device`) не
        // должны падать при чтении — см. doc-комментарий на самом поле.
        let json = r#"{"backend":"Pipewire"}"#;
        let cfg: AudioConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.device, AudioDevice::VirtioSound);
    }
}
