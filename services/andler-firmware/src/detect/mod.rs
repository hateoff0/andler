//! Hardware/host auto-detection, orchestrated by [`detect_all`].
//!
//! Individual `detect_*` functions live in per-topic submodules and are
//! `pub(crate)` — the wizard (and anything else outside this crate) should
//! only ever call [`detect_all`] and read the resulting [`HardwareDefaults`],
//! never the individual detectors directly. This keeps "what gets detected
//! and in what order" defined in exactly one place.
//!
//! All checks are syscall-based (`Path::exists`, `std::process::Command`,
//! file reads) — no network I/O, nothing that can block for long. Results
//! are **not** cached: each call to `detect_all()` re-detects from
//! scratch, so it reflects the current state of the host (e.g. if PipeWire
//! was started after the daemon booted).

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

/// Auto-detected hardware/host defaults, consumed by the wizard's
/// `advanced.rs` to pre-fill every hardware-dependent question. See
/// WIZARD.md, "HardwareDefaults — auto-detected values from
/// andler-firmware" for the field-by-field contract.
#[derive(Debug)]
pub struct HardwareDefaults {
    /// OVMF CODE+VARS paths, or the reason none could be found (surfaced
    /// to the wizard/daemon as an actionable error, not silently ignored).
    pub ovmf: Result<DetectedOvmf, FirmwareError>,
    /// Venus/VirGL/CPU — auto-detected from GPU vendor + host capability.
    pub gpu_render: RenderBackend,
    /// SDL/GTK — auto-detected from GPU vendor (GTK misbehaves on some
    /// NVIDIA configurations, see `detect::gpu`).
    pub display_engine: DisplayEngine,
    /// PipeWire/PulseAudio/None — auto-detected from host session sockets.
    pub audio_server: AudioServer,
    /// Libndk/Libhoudini/None — auto-detected from `/proc/cpuinfo` vendor.
    pub arm_translator: Option<ArmTranslator>,
    /// `true` if the host meets Venus's kernel/QEMU/Mesa version floor.
    pub venus_supported: bool,
    /// `true` if the `passt` binary is installed.
    pub passt_available: bool,
}

/// Runs every detector once and assembles [`HardwareDefaults`]. This is
/// the only entry point the wizard should call — see the module doc.
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
