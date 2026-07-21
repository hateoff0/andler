

use andler_core::AudioBackend;


pub type AudioServer = AudioBackend;


pub(crate) fn detect_audio_server() -> AudioServer {
    match runtime_dir() {
        Some(dir) => detect_from_runtime_dir(&dir),
        None => AudioServer::None,
    }
}

fn detect_from_runtime_dir(runtime_dir: &str) -> AudioServer {
    if std::path::Path::new(&format!("{runtime_dir}/pipewire-0")).exists() {
        return AudioServer::Pipewire;
    }
    if std::path::Path::new(&format!("{runtime_dir}/pulse/native")).exists() {
        return AudioServer::Pulseaudio;
    }
    AudioServer::None
}


fn runtime_dir() -> Option<String> {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return Some(dir);
        }
    }

    let uid = current_uid();
    let candidate = format!("/run/user/{uid}");
    if std::path::Path::new(&candidate).exists() {
        Some(candidate)
    } else {
        None
    }
}


fn current_uid() -> u32 {
    andler_core::paths::current_uid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_runtime_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("andler-firmware-test-audio-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_pipewire_socket() {
        let dir = temp_runtime_dir("pipewire");
        std::fs::write(dir.join("pipewire-0"), b"").unwrap();
        assert_eq!(
            detect_from_runtime_dir(dir.to_str().unwrap()),
            AudioServer::Pipewire
        );
    }

    #[test]
    fn detects_pulseaudio_socket_when_no_pipewire() {
        let dir = temp_runtime_dir("pulseaudio");
        std::fs::create_dir_all(dir.join("pulse")).unwrap();
        std::fs::write(dir.join("pulse/native"), b"").unwrap();
        assert_eq!(
            detect_from_runtime_dir(dir.to_str().unwrap()),
            AudioServer::Pulseaudio
        );
    }

    #[test]
    fn prefers_pipewire_over_pulseaudio_when_both_present() {
        let dir = temp_runtime_dir("both");
        std::fs::write(dir.join("pipewire-0"), b"").unwrap();
        std::fs::create_dir_all(dir.join("pulse")).unwrap();
        std::fs::write(dir.join("pulse/native"), b"").unwrap();
        assert_eq!(
            detect_from_runtime_dir(dir.to_str().unwrap()),
            AudioServer::Pipewire
        );
    }

    #[test]
    fn returns_none_when_neither_socket_present() {
        let dir = temp_runtime_dir("empty");
        assert_eq!(detect_from_runtime_dir(dir.to_str().unwrap()), AudioServer::None);
    }

    #[test]
    fn returns_none_when_runtime_dir_does_not_exist() {
        assert_eq!(
            detect_from_runtime_dir("/nonexistent/andler-firmware-test-path"),
            AudioServer::None
        );
    }
}
