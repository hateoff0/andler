

use andler_core::{
    AudioBackend, CdromBus, NetworkMode, PointerMode, RenderBackend, Resolution,
};
use andler_firmware::HardwareDefaults;
use inquire::{Confirm, CustomType, Select, Text};

use crate::CliArmTranslator;

use super::basic::{AndroidBasicResult, LinuxBasicResult};
use super::{map_inquire_err, WizardError};

const MIN_GPU_MEMORY_MIB: u64 = 256;
const MAX_GPU_MEMORY_MIB: u64 = 16384;
const MIN_RESOLUTION: u32 = 64;
const MAX_RESOLUTION: u32 = 7680;
const MIN_CPU_CORES: u32 = 1;
const MAX_CPU_CORES: u32 = 128;
const MIN_MEMORY_GIB: u64 = 1;
const MAX_MEMORY_GIB: u64 = 1024;

#[derive(Debug, Clone)]
pub struct AdvancedConfig {
    pub cdrom_bus: Option<CdromBus>,
    pub compact_on_shutdown: bool,
    pub gpu_render: RenderBackend,
    pub gpu_memory_mib: u64,
    pub display_resolution: Resolution,
    pub fullscreen: bool,
    pub audio_backend: AudioBackend,
    pub clipboard_enabled: bool,
    pub input_pointer: PointerMode,
    pub cpu_cores: u32,
    pub memory_gib: u64,
    pub arm_translator: Option<CliArmTranslator>,
    pub gapps: bool,
    pub microg: bool,
    pub network_mode: NetworkMode,
    pub bridge_interface: Option<String>,
}

pub fn run_linux(
    result: &LinuxBasicResult,
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
) -> Result<AdvancedConfig, WizardError> {
    let pref = prefilled;

    let cdrom_bus = if result.iso_path.is_empty() {
        None
    } else {
        let recommended =
            CdromBus::recommended_for_iso_filename(std::path::Path::new(&result.iso_path));
        Some(ask_cdrom_bus(
            std::path::Path::new(&result.iso_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&result.iso_path),
            recommended,
            pref.and_then(|p| p.cdrom_bus),
        )?)
    };

    let network_mode = ask_network_mode(pref.map(|p| p.network_mode.clone()))?;
    let bridge_interface = if let NetworkMode::Bridge { .. } = network_mode {
        ask_bridge_interface(pref.and_then(|p| p.bridge_interface.clone()))?
    } else {
        None
    };

    Ok(AdvancedConfig {
        cdrom_bus,
        compact_on_shutdown: ask_compact_on_shutdown(pref.map(|p| p.compact_on_shutdown))?,
        gpu_render: ask_gpu_render(detected, pref.map(|p| p.gpu_render.clone()))?,
        gpu_memory_mib: ask_gpu_memory(pref.map(|p| p.gpu_memory_mib))?,
        display_resolution: ask_display_resolution(pref.map(|p| p.display_resolution))?,
        fullscreen: ask_fullscreen(pref.map(|p| p.fullscreen))?,
        audio_backend: ask_audio_backend(detected, pref.map(|p| p.audio_backend))?,
        clipboard_enabled: ask_clipboard_enabled(pref.map(|p| p.clipboard_enabled))?,
        input_pointer: ask_input_pointer(pref.map(|p| p.input_pointer))?,
        cpu_cores: ask_cpu_cores(pref.map(|p| p.cpu_cores))?,
        memory_gib: ask_memory_gib(pref.map(|p| p.memory_gib))?,
        arm_translator: None,
        gapps: false,
        microg: false,
        network_mode,
        bridge_interface,
    })
}

pub fn run_android(
    _result: &AndroidBasicResult,
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
) -> Result<AdvancedConfig, WizardError> {
    let pref = prefilled;

    let gapps = ask_gapps(pref.map(|p| p.gapps))?;
    let microg = if gapps {
        false
    } else {
        ask_microg(pref.map(|p| p.microg))?
    };
    let network_mode = ask_network_mode(pref.map(|p| p.network_mode.clone()))?;
    let bridge_interface = if let NetworkMode::Bridge { .. } = network_mode {
        ask_bridge_interface(pref.and_then(|p| p.bridge_interface.clone()))?
    } else {
        None
    };

    Ok(AdvancedConfig {
        cdrom_bus: None,
        compact_on_shutdown: ask_compact_on_shutdown(pref.map(|p| p.compact_on_shutdown))?,
        gpu_render: ask_gpu_render(detected, pref.map(|p| p.gpu_render.clone()))?,
        gpu_memory_mib: ask_gpu_memory(pref.map(|p| p.gpu_memory_mib))?,
        display_resolution: ask_display_resolution(pref.map(|p| p.display_resolution))?,
        fullscreen: ask_fullscreen(pref.map(|p| p.fullscreen))?,
        audio_backend: ask_audio_backend(detected, pref.map(|p| p.audio_backend))?,
        clipboard_enabled: ask_clipboard_enabled(pref.map(|p| p.clipboard_enabled))?,
        input_pointer: ask_input_pointer(pref.map(|p| p.input_pointer))?,
        cpu_cores: ask_cpu_cores(pref.map(|p| p.cpu_cores))?,
        memory_gib: ask_memory_gib(pref.map(|p| p.memory_gib))?,
        arm_translator: Some(ask_arm_translator(
            detected,
            pref.and_then(|p| p.arm_translator),
        )?),
        gapps,
        microg,
        network_mode,
        bridge_interface,
    })
}

fn ask_cdrom_bus(
    _iso_name: &str,
    recommended: CdromBus,
    prefilled: Option<CdromBus>,
) -> Result<CdromBus, WizardError> {
    if let Some(bus) = prefilled {
        return Ok(bus);
    }

    let rec_str = match recommended {
        CdromBus::VirtioScsi => "virtio-scsi",
        CdromBus::Ide => "ide",
    };

    let virtio = "virtio-scsi (faster, modern distro initrds support it)";
    let ide = "ide (compatible with Windows and any unknown ISO)";
    let options = if recommended == CdromBus::VirtioScsi {
        vec![virtio, ide]
    } else {
        vec![ide, virtio]
    };

    let choice = Select::new(
        &format!("Installation media detected. CD-ROM bus (auto: {rec_str}):"),
        options,
    )
    .with_help_message(
        "virtio-scsi — faster, modern distro initrds support it; \
         ide — compatible with Windows and any unknown ISO",
    )
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("virtio") {
        CdromBus::VirtioScsi
    } else {
        CdromBus::Ide
    })
}

fn ask_compact_on_shutdown(prefilled: Option<bool>) -> Result<bool, WizardError> {
    Confirm::new("Compact disk after shutdown?")
        .with_default(prefilled.unwrap_or(false))
        .with_help_message(
            "Saves space but rewrites entire disk file — may take time on large disks",
        )
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_gpu_render(
    detected: &HardwareDefaults,
    prefilled: Option<RenderBackend>,
) -> Result<RenderBackend, WizardError> {
    let default = prefilled.unwrap_or_else(|| detected.gpu_render.clone());

    let venus = "Venus (3D via Vulkan, fastest)";
    let virgl = "VirGL (OpenGL 3D, broader compatibility)";
    let virtio = "VirtioGPU (2D only)";
    let cpu = "CPU (software rendering)";

    let mut options = vec![virgl, virtio, cpu];
    if detected.venus_supported {
        options.insert(0, venus);
    }

    let default_label = render_label(default);
    let prompt_options: Vec<String> = options
        .iter()
        .map(|o| {
            if *o == default_label {
                format!("{o} (recommended)")
            } else {
                (*o).to_string()
            }
        })
        .collect();

    let choice = Select::new("GPU render:", prompt_options.clone())
        .with_help_message(
            "Venus — Vulkan 3D (fastest); VirGL — OpenGL 3D (broader); \
             VirtioGPU — 2D; CPU — software",
        )
        .with_starting_cursor(
            prompt_options
                .iter()
                .position(|o| o.starts_with(default_label))
                .unwrap_or(0),
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(parse_render_choice(&choice))
}

fn ask_gpu_memory(prefilled: Option<u64>) -> Result<u64, WizardError> {
    CustomType::<u64>::new("GPU memory (MiB):")
        .with_default(prefilled.unwrap_or(4096))
        .with_help_message(
            "Host memory allocated for GPU device. 4096 MiB is sufficient for most workloads.",
        )
        .with_error_message("Enter an integer between 256 and 16384, e.g. 4096")
        .with_validator(|v: &u64| {
            if (MIN_GPU_MEMORY_MIB..=MAX_GPU_MEMORY_MIB).contains(v) {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    format!(
                        "Enter an integer between {MIN_GPU_MEMORY_MIB} and {MAX_GPU_MEMORY_MIB}, e.g. 4096"
                    )
                    .into(),
                ))
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_display_resolution(prefilled: Option<Resolution>) -> Result<Resolution, WizardError> {
    let default = prefilled
        .map(|r| format!("{}x{}", r.width, r.height))
        .unwrap_or_else(|| "1920x1080".to_string());

    let raw = Text::new("Display resolution (e.g. 1920x1080):")
        .with_default(&default)
        .with_help_message(
            "Initial screen resolution. Format: WIDTHxHEIGHT (e.g. 1920x1080, 2560x1440). \
             Note: not yet applied to the actual display output (QEMU's SDL/GTK backends \
             don't take a resolution parameter) -- set it in the guest OS after boot for now. \
             Note: not yet applied to the actual display output (QEMU's SDL/GTK backends \
             don't take a resolution parameter) -- set it in the guest OS after boot for now."
        )
        .with_validator(|s: &str| match parse_resolution(s) {
            Ok(_) => Ok(inquire::validator::Validation::Valid),
            Err(e) => Ok(inquire::validator::Validation::Invalid(e.into())),
        })
        .prompt()
        .map_err(map_inquire_err)?;

    parse_resolution(&raw).map_err(WizardError::Inquire)
}

fn ask_fullscreen(prefilled: Option<bool>) -> Result<bool, WizardError> {
    Confirm::new("Start in fullscreen mode?")
        .with_default(prefilled.unwrap_or(false))
        .with_help_message("Start the VM window in fullscreen mode.")
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_audio_backend(
    detected: &HardwareDefaults,
    prefilled: Option<AudioBackend>,
) -> Result<AudioBackend, WizardError> {
    let default = prefilled.unwrap_or(detected.audio_server);

    let pipewire = "PipeWire (auto-detected)";
    let pulse = "PulseAudio (auto-detected)";
    let none = "None (no audio)";

    let (options, default_idx) = match default {
        AudioBackend::Pipewire => (vec![pipewire, pulse, none], 0),
        AudioBackend::Pulseaudio => (vec![pulse, pipewire, none], 0),
        AudioBackend::None => (vec![none, pipewire, pulse], 0),
    };

    let choice = Select::new("Audio backend:", options)
        .with_help_message(
            "PipeWire — modern, recommended; PulseAudio — legacy; None — no audio",
        )
        .with_starting_cursor(default_idx)
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(parse_audio_choice(&choice))
}

fn ask_clipboard_enabled(prefilled: Option<bool>) -> Result<bool, WizardError> {
    Confirm::new("Enable clipboard sharing between host and VM?")
        .with_default(prefilled.unwrap_or(true))
        .with_help_message("Enable copy-paste between host and VM via qemu-vdagent.")
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_input_pointer(prefilled: Option<PointerMode>) -> Result<PointerMode, WizardError> {
    let default = prefilled.unwrap_or(PointerMode::Tablet);
    let tablet = "tablet (absolute coordinates, recommended)";
    let mouse = "mouse (relative coordinates)";

    let options = if default == PointerMode::Tablet {
        vec![tablet, mouse]
    } else {
        vec![mouse, tablet]
    };

    let choice = Select::new("Input pointer:", options)
        .with_help_message(
            "tablet — absolute coordinates (recommended); mouse — relative coordinates",
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("mouse") {
        PointerMode::Mouse
    } else {
        PointerMode::Tablet
    })
}

fn ask_cpu_cores(prefilled: Option<u32>) -> Result<u32, WizardError> {
    CustomType::<u32>::new("CPU cores:")
        .with_default(prefilled.unwrap_or(4))
        .with_help_message("Number of virtual CPUs. Default 4 is sufficient for most use cases.")
        .with_error_message("Enter an integer between 1 and 128")
        .with_validator(|v: &u32| {
            if (MIN_CPU_CORES..=MAX_CPU_CORES).contains(v) {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    format!("Enter an integer between {MIN_CPU_CORES} and {MAX_CPU_CORES}").into(),
                ))
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_memory_gib(prefilled: Option<u64>) -> Result<u64, WizardError> {
    CustomType::<u64>::new("Memory (GiB):")
        .with_default(prefilled.unwrap_or(8))
        .with_help_message("RAM in GiB. Default 8 is sufficient for most use cases.")
        .with_error_message("Enter an integer between 1 and 1024, e.g. 8")
        .with_validator(|v: &u64| {
            if (MIN_MEMORY_GIB..=MAX_MEMORY_GIB).contains(v) {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    format!(
                        "Enter an integer between {MIN_MEMORY_GIB} and {MAX_MEMORY_GIB}, e.g. 8"
                    )
                    .into(),
                ))
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_arm_translator(
    detected: &HardwareDefaults,
    prefilled: Option<CliArmTranslator>,
) -> Result<CliArmTranslator, WizardError> {
    let auto = detected.arm_translator.map(|t| match t {
        andler_core::ArmTranslator::Libndk => "libndk (auto)",
        andler_core::ArmTranslator::Libhoudini => "libhoudini (auto)",
        andler_core::ArmTranslator::None => "none (auto)",
    });

    let default = prefilled.or_else(|| {
        detected.arm_translator.map(|t| match t {
            andler_core::ArmTranslator::Libndk => CliArmTranslator::Libndk,
            andler_core::ArmTranslator::Libhoudini => CliArmTranslator::Libhoudini,
            andler_core::ArmTranslator::None => CliArmTranslator::None,
        })
    });

    let auto_hint = auto.unwrap_or("none (auto)");
    let none = "none";
    let libndk = "libndk (AMD)";
    let libhoudini = "libhoudini (Intel)";

    let default_label = match default {
        Some(CliArmTranslator::Libndk) => libndk,
        Some(CliArmTranslator::Libhoudini) => libhoudini,
        _ => none,
    };

    let prompt = format!("ARM translator (auto: {auto_hint}):");
    let mut ordered = vec![default_label.to_string()];
    for opt in [none, libndk, libhoudini] {
        if opt != default_label {
            ordered.push(opt.to_string());
        }
    }

    let choice = Select::new(&prompt, ordered)
        .with_help_message(
            "libndk — for AMD CPUs; libhoudini — for Intel CPUs; none — no ARM app support",
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(parse_arm_translator(&choice).unwrap_or(CliArmTranslator::None))
}

fn ask_gapps(prefilled: Option<bool>) -> Result<bool, WizardError> {
    Confirm::new("Enable GApps?")
        .with_default(prefilled.unwrap_or(false))
        .with_help_message(
            "Google Play Store and Google services. Requires internet to set up on first boot.",
        )
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_microg(prefilled: Option<bool>) -> Result<bool, WizardError> {
    Confirm::new("Enable MicroG?")
        .with_default(prefilled.unwrap_or(false))
        .with_help_message("Open-source Google Play replacement. No Google account needed.")
        .prompt()
        .map_err(map_inquire_err)
}

fn render_label(backend: RenderBackend) -> &'static str {
    match backend {
        RenderBackend::Venus => "Venus (3D via Vulkan, fastest)",
        RenderBackend::VirGl => "VirGL (OpenGL 3D, broader compatibility)",
        RenderBackend::VirtioGpu => "VirtioGPU (2D only)",
        RenderBackend::Cpu => "CPU (software rendering)",
        RenderBackend::Passthrough { .. } => "CPU (software rendering)",
    }
}

fn parse_render_choice(choice: &str) -> RenderBackend {
    if choice.starts_with("Venus") {
        RenderBackend::Venus
    } else if choice.starts_with("VirGL") {
        RenderBackend::VirGl
    } else if choice.starts_with("VirtioGPU") {
        RenderBackend::VirtioGpu
    } else {
        RenderBackend::Cpu
    }
}

fn parse_audio_choice(choice: &str) -> AudioBackend {
    if choice.starts_with("Pulse") {
        AudioBackend::Pulseaudio
    } else if choice.starts_with("None") {
        AudioBackend::None
    } else {
        AudioBackend::Pipewire
    }
}

pub fn parse_arm_translator(s: &str) -> Option<CliArmTranslator> {
    match s.split_whitespace().next()? {
        "libndk" => Some(CliArmTranslator::Libndk),
        "libhoudini" => Some(CliArmTranslator::Libhoudini),
        "none" => Some(CliArmTranslator::None),
        _ => None,
    }
}

pub fn parse_resolution(s: &str) -> Result<Resolution, String> {
    let s = s.trim();
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| "Invalid format. Use WIDTHxHEIGHT, e.g. 1920x1080".to_string())?;
    let width: u32 = w
        .trim()
        .parse()
        .map_err(|_| "Invalid width".to_string())?;
    let height: u32 = h
        .trim()
        .parse()
        .map_err(|_| "Invalid height".to_string())?;
    if !(MIN_RESOLUTION..=MAX_RESOLUTION).contains(&width)
        || !(MIN_RESOLUTION..=MAX_RESOLUTION).contains(&height)
    {
        return Err(format!(
            "Width and height must be between {MIN_RESOLUTION} and {MAX_RESOLUTION}"
        ));
    }
    Ok(Resolution::new(width, height))
}

#[allow(dead_code)] // reserved for future GPU memory configuration in the wizard
pub fn parse_gpu_memory(s: &str) -> Result<u64, String> {
    let v: u64 = s.trim().parse().map_err(|_| "Invalid number".to_string())?;
    if (MIN_GPU_MEMORY_MIB..=MAX_GPU_MEMORY_MIB).contains(&v) {
        Ok(v)
    } else {
        Err(format!(
            "Enter an integer between {MIN_GPU_MEMORY_MIB} and {MAX_GPU_MEMORY_MIB}, e.g. 4096"
        ))
    }
}


fn ask_network_mode(_prefilled: Option<NetworkMode>) -> Result<NetworkMode, WizardError> {
    let options = vec!["NAT (default)", "Bridge", "Isolated"];
    let selection = Select::new("Network mode:", options)
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(match &*selection {
        "NAT (default)" | "NAT" => NetworkMode::Nat,
        "Bridge" => NetworkMode::Bridge { interface: String::new() },
        "Isolated" => NetworkMode::Isolated,
        _ => unreachable!(),
    })
}


fn ask_bridge_interface(prefilled: Option<String>) -> Result<Option<String>, WizardError> {
    let prompt = Text::new("Bridge interface name (e.g., br0):")
        .with_placeholder("br0")
        .with_validator(|input: &str| {
            if input.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    inquire::validator::ErrorMessage::Custom("Bridge interface name cannot be empty".to_string()),
                ))
            } else if !input.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                Ok(inquire::validator::Validation::Invalid(
                    inquire::validator::ErrorMessage::Custom("Interface name can only contain letters, numbers, hyphens, and underscores".to_string()),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .with_default(&prefilled.unwrap_or_default())
        .prompt()
        .map_err(map_inquire_err)?;

    if prompt.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(prompt.trim().to_string()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arm_translator_valid() {
        assert_eq!(
            parse_arm_translator("libndk (AMD)"),
            Some(CliArmTranslator::Libndk)
        );
    }

    #[test]
    fn parse_arm_translator_invalid() {
        assert_eq!(parse_arm_translator("unknown"), None);
    }

    #[test]
    fn parse_resolution_valid() {
        let r = parse_resolution("1920x1080").unwrap();
        assert_eq!(r.width, 1920);
        assert_eq!(r.height, 1080);
    }

    #[test]
    fn parse_resolution_invalid_format() {
        assert!(parse_resolution("1920").is_err());
    }

    #[test]
    fn parse_resolution_out_of_range() {
        assert!(parse_resolution("8192x8192").is_err());
    }

    #[test]
    fn parse_gpu_memory_valid() {
        assert_eq!(parse_gpu_memory("4096").unwrap(), 4096);
    }

    #[test]
    fn parse_gpu_memory_too_low() {
        assert!(parse_gpu_memory("128").is_err());
    }
}
