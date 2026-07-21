

mod arm;
mod audio;
mod gpu;
mod network;
mod ovmf;

pub use ovmf::{
    detect, detect_matched_pair, provision_vars, reset_vars, DetectedOvmf,
    KNOWN_OVMF_CODE_PATHS, KNOWN_OVMF_VARS_PATHS,
};

pub use audio::AudioServer;

use andler_core::{ArmTranslator, DisplayEngine, RenderBackend};

use crate::error::FirmwareError;


#[derive(Debug)]
pub struct HardwareDefaults {

    pub ovmf: Result<DetectedOvmf, FirmwareError>,

    pub gpu_render: RenderBackend,

    pub display_engine: DisplayEngine,

    pub audio_server: AudioServer,

    pub arm_translator: Option<ArmTranslator>,

    pub venus_supported: bool,

    pub passt_available: bool,
}


pub fn detect_all() -> HardwareDefaults {
    let ovmf = detect_matched_pair();
    let (gpu_render, display_engine, venus_supported) = gpu::detect_gpu_defaults();
    let audio_server = audio::detect_audio_server();
    let arm_translator = arm::detect_arm_translator();
    let passt_available = network::detect_passt_available();

    tracing::debug!(
        gpu_render = ?gpu_render,
        display_engine = ?display_engine,
        venus_supported,
        "GPU detected"
    );
    tracing::debug!(audio_server = ?audio_server, "audio server detected");
    tracing::debug!(arm_translator = ?arm_translator, "ARM translator detected");
    tracing::debug!(passt_available, "passt availability detected");
    match &ovmf {
        Ok(found) => tracing::debug!(code = ?found.code, vars = ?found.vars_template, "OVMF detected"),
        Err(err) => tracing::debug!(error = %err, "OVMF not found"),
    }

    HardwareDefaults {
        ovmf,
        gpu_render,
        display_engine,
        audio_server,
        arm_translator,
        venus_supported,
        passt_available,
    }
}
