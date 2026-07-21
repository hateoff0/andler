

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerMode {

    Tablet,

    Mouse,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {

    #[serde(default = "default_pointer_mode")]
    pub pointer_mode: PointerMode,

    pub hide_host_cursor: bool,

    pub clipboard_enabled: bool,
}

fn default_pointer_mode() -> PointerMode {
    PointerMode::Tablet
}

impl InputConfig {

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
