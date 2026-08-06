use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioBackend {
    Pipewire,
    Pulseaudio,

    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioDevice {
    VirtioSound,

    Ich9Hda,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub backend: AudioBackend,

    #[serde(default = "default_audio_device")]
    pub device: AudioDevice,
}

fn default_audio_device() -> AudioDevice {
    AudioDevice::VirtioSound
}

impl AudioConfig {
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
        let cfg = AudioConfig::reference_default();
        assert_eq!(cfg.device, AudioDevice::VirtioSound);
    }

    #[test]
    fn device_field_deserializes_with_default_when_missing() {
        let json = r#"{"backend":"Pipewire"}"#;
        let cfg: AudioConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.device, AudioDevice::VirtioSound);
    }
}
