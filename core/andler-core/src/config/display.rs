use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Resolution { width, height }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayEngine {
    Sdl,

    Gtk,

    Spice,

    Dbus,

    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayConfig {
    pub resolution: Resolution,
    pub dpi: u32,

    pub fps_limit: u32,
    pub display_engine: DisplayEngine,
    pub fullscreen: bool,
}

impl DisplayConfig {
    pub const FPS_UNLIMITED: u32 = 0;

    pub fn reference_default() -> Self {
        DisplayConfig {
            resolution: Resolution::new(1920, 1080),
            dpi: 96,
            fps_limit: Self::FPS_UNLIMITED,
            display_engine: DisplayEngine::Sdl,
            fullscreen: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_uses_sdl() {
        let cfg = DisplayConfig::reference_default();
        assert_eq!(cfg.display_engine, DisplayEngine::Sdl);
        assert_eq!(cfg.fps_limit, DisplayConfig::FPS_UNLIMITED);
        assert!(!cfg.fullscreen);
    }

    #[test]
    fn none_display_engine_round_trips_through_serde_json() {
        let cfg = DisplayConfig {
            display_engine: DisplayEngine::None,
            ..DisplayConfig::reference_default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DisplayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
