

pub mod detect;
pub mod error;
pub mod metrics;

pub use detect::{
    detect, detect_all, detect_matched_pair, provision_vars, reset_vars, AudioServer,
    DetectedOvmf, HardwareDefaults, KNOWN_OVMF_CODE_PATHS, KNOWN_OVMF_VARS_PATHS,
};
pub use error::FirmwareError;
