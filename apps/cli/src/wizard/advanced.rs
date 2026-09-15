use andler_core::{AudioBackend, CdromBus, NetworkMode, PointerMode, RenderBackend, Resolution};
use andler_firmware::HardwareDefaults;

use crate::CliArmTranslator;

use super::basic::{AndroidBasicResult, LinuxBasicResult};
use super::ui;
use super::WizardError;

const MIN_GPU_MEMORY_MIB: u64 = 256;
const MAX_GPU_MEMORY_MIB: u64 = 16384;
const MIN_RESOLUTION: u32 = 64;
const MAX_RESOLUTION: u32 = 7680;
const MIN_CPU_CORES: u32 = 1;
const MAX_CPU_CORES: u32 = 128;
const MIN_MEMORY_GIB: u64 = 1;
const MAX_MEMORY_GIB: u64 = 1024;

/// The advanced answers, in the shape the request builders consume.
///
/// `Default` is the wizard's recommended configuration; the first pass fills
/// every field from the user's answers (falling back to hardware detection),
/// and the "modify" pass rewrites only the groups the user picks.
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
    pub network_mode: NetworkMode,
    pub bridge_interface: Option<String>,
    pub linked_overlay: bool,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            cdrom_bus: None,
            compact_on_shutdown: false,
            gpu_render: RenderBackend::Venus,
            gpu_memory_mib: 4096,
            display_resolution: Resolution::new(1920, 1080),
            fullscreen: false,
            audio_backend: AudioBackend::Pipewire,
            clipboard_enabled: true,
            input_pointer: PointerMode::Tablet,
            cpu_cores: 4,
            memory_gib: 8,
            arm_translator: None,
            gapps: false,
            network_mode: NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        }
    }
}

/// One screenful of related questions. Re-asking a whole group beats
/// re-asking every question when the user came back to change one thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    BootDisks,
    DisplayGpu,
    Devices,
    CpuMemory,
    Network,
    Android,
}

impl Group {
    fn label(&self) -> &'static str {
        match self {
            Group::BootDisks => "Boot & disks",
            Group::DisplayGpu => "Display & GPU",
            Group::Devices => "Devices",
            Group::CpuMemory => "CPU & memory",
            Group::Network => "Network",
            Group::Android => "Android",
        }
    }

    /// What the group owns, shown while its entry is highlighted: six groups
    /// have to fit one screen, so the label stays short and the detail lives
    /// here instead of in the label.
    fn hint(&self) -> &'static str {
        match self {
            Group::BootDisks => "CD-ROM bus, compact on shutdown",
            Group::DisplayGpu => "render backend, GPU memory, resolution, fullscreen",
            Group::Devices => "audio, clipboard, input pointer",
            Group::CpuMemory => "cores, RAM",
            Group::Network => "NAT, bridge or isolated",
            Group::Android => "GApps, ARM translator, disk overlay",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Group::BootDisks => "boot & disks",
            Group::DisplayGpu => "display & gpu",
            Group::Devices => "devices",
            Group::CpuMemory => "cpu & memory",
            Group::Network => "network",
            Group::Android => "android",
        }
    }
}

fn ask_groups(groups: &[Group]) -> Result<Vec<Group>, WizardError> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let mut prompt = cliclack::multiselect(
        "which settings do you want to change?\nspace toggles, Enter confirms; \
         empty keeps the current answers",
    )
    .required(false);
    for group in groups {
        prompt = prompt.item(*group, group.label(), group.hint());
    }

    Ok(prompt.interact()?)
}

pub fn run_linux(
    result: &LinuxBasicResult,
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
) -> Result<AdvancedConfig, WizardError> {
    ui::step("advanced settings")?;

    let mut config = prefilled.cloned().unwrap_or_default();
    let wanted = wanted_groups(
        &[
            Group::BootDisks,
            Group::DisplayGpu,
            Group::Devices,
            Group::CpuMemory,
            Group::Network,
        ],
        prefilled,
    )?;

    ask_boot_disks(result, prefilled, &mut config, &wanted)?;
    ask_display_gpu(detected, prefilled, &mut config, &wanted)?;
    ask_devices(detected, prefilled, &mut config, &wanted)?;
    ask_cpu_memory(prefilled, &mut config, &wanted)?;
    ask_network(prefilled, &mut config, &wanted)?;

    Ok(config)
}

pub fn run_android(
    result: &AndroidBasicResult,
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
) -> Result<AdvancedConfig, WizardError> {
    ui::step("advanced settings")?;

    let mut config = prefilled.cloned().unwrap_or_default();
    let wanted = wanted_groups(
        &[
            Group::Android,
            Group::DisplayGpu,
            Group::Devices,
            Group::CpuMemory,
            Group::Network,
        ],
        prefilled,
    )?;

    ask_android(result, detected, prefilled, &mut config, &wanted)?;
    ask_display_gpu(detected, prefilled, &mut config, &wanted)?;
    ask_devices(detected, prefilled, &mut config, &wanted)?;
    ask_cpu_memory(prefilled, &mut config, &wanted)?;
    ask_network(prefilled, &mut config, &wanted)?;

    Ok(config)
}

/// First pass: everything. Modify pass: only what the user picked.
fn wanted_groups(
    all: &[Group],
    prefilled: Option<&AdvancedConfig>,
) -> Result<Vec<Group>, WizardError> {
    match prefilled {
        None => Ok(all.to_vec()),
        Some(_) => ask_groups(all),
    }
}

fn ask_group(wanted: &[Group], group: Group) -> bool {
    wanted.contains(&group)
}

fn ask_android(
    result: &AndroidBasicResult,
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::Android) {
        return Ok(());
    }
    ui::step(Group::Android.title())?;

    config.gapps = ask_gapps(prefilled.map(|p| p.gapps).or(Some(result.gapps)))?;
    config.arm_translator = Some(ask_arm_translator(
        detected,
        prefilled.and_then(|p| p.arm_translator),
    )?);
    config.linked_overlay = ask_linked_overlay(prefilled.map(|p| p.linked_overlay))?;
    Ok(())
}

fn ask_boot_disks(
    result: &LinuxBasicResult,
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::BootDisks) {
        return Ok(());
    }
    ui::step(Group::BootDisks.title())?;

    let recommended =
        CdromBus::recommended_for_iso_filename(std::path::Path::new(&result.iso_path));
    config.cdrom_bus = Some(ask_cdrom_bus(
        recommended,
        prefilled.and_then(|p| p.cdrom_bus),
    )?);
    config.compact_on_shutdown = ask_compact_on_shutdown(prefilled.map(|p| p.compact_on_shutdown))?;
    Ok(())
}

fn ask_display_gpu(
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::DisplayGpu) {
        return Ok(());
    }
    ui::step(Group::DisplayGpu.title())?;

    config.gpu_render = ask_gpu_render(detected, prefilled.map(|p| p.gpu_render.clone()))?;
    config.gpu_memory_mib = ask_gpu_memory(prefilled.map(|p| p.gpu_memory_mib))?;
    config.display_resolution = ask_display_resolution(prefilled.map(|p| p.display_resolution))?;
    config.fullscreen = ask_fullscreen(prefilled.map(|p| p.fullscreen))?;
    Ok(())
}

fn ask_devices(
    detected: &HardwareDefaults,
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::Devices) {
        return Ok(());
    }
    ui::step(Group::Devices.title())?;

    config.audio_backend = ask_audio_backend(detected, prefilled.map(|p| p.audio_backend))?;
    config.clipboard_enabled = ask_clipboard_enabled(prefilled.map(|p| p.clipboard_enabled))?;
    config.input_pointer = ask_input_pointer(prefilled.map(|p| p.input_pointer))?;
    Ok(())
}

fn ask_cpu_memory(
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::CpuMemory) {
        return Ok(());
    }
    ui::step(Group::CpuMemory.title())?;

    config.cpu_cores = ask_cpu_cores(prefilled.map(|p| p.cpu_cores))?;
    config.memory_gib = ask_memory_gib(prefilled.map(|p| p.memory_gib))?;
    Ok(())
}

fn ask_network(
    prefilled: Option<&AdvancedConfig>,
    config: &mut AdvancedConfig,
    wanted: &[Group],
) -> Result<(), WizardError> {
    if !ask_group(wanted, Group::Network) {
        return Ok(());
    }
    ui::step(Group::Network.title())?;

    config.network_mode = ask_network_mode(prefilled.map(|p| p.network_mode.clone()))?;
    config.bridge_interface = if let NetworkMode::Bridge { .. } = config.network_mode {
        ask_bridge_interface(prefilled.and_then(|p| p.bridge_interface.clone()))?
    } else {
        None
    };
    Ok(())
}

fn ask_cdrom_bus(
    recommended: CdromBus,
    prefilled: Option<CdromBus>,
) -> Result<CdromBus, WizardError> {
    // The prefilled answer is the default, exactly like every other question:
    // returning it straight back made the modify pass unable to change this
    // one group's first answer.
    let bus = cliclack::select(format!(
        "CD-ROM bus (auto: {})",
        ui::cdrom_bus_label(recommended)
    ))
    .item(
        CdromBus::VirtioScsi,
        "virtio-scsi",
        "faster — modern distro initrds support it",
    )
    .item(
        CdromBus::Ide,
        "ide",
        "compatible with Windows and any unknown ISO",
    )
    .initial_value(prefilled.unwrap_or(recommended))
    .interact()?;

    Ok(bus)
}

fn ask_compact_on_shutdown(prefilled: Option<bool>) -> Result<bool, WizardError> {
    let compact = cliclack::confirm(
        "compact the disk after shutdown?\nsaves space, but rewrites the whole file",
    )
    .initial_value(prefilled.unwrap_or(false))
    .interact()?;
    Ok(compact)
}

fn ask_gpu_render(
    detected: &HardwareDefaults,
    prefilled: Option<RenderBackend>,
) -> Result<RenderBackend, WizardError> {
    let default = prefilled.unwrap_or_else(|| detected.gpu_render.clone());

    let mut prompt = cliclack::select(format!(
        "GPU render backend (detected: {})",
        ui::render_label(&detected.gpu_render)
    ));
    if detected.venus_supported {
        prompt = prompt.item(RenderBackend::Venus, "Venus", "Vulkan 3D — fastest");
    }
    prompt = prompt
        .item(
            RenderBackend::VirGl,
            "VirGL",
            "OpenGL 3D — broader compatibility",
        )
        .item(RenderBackend::VirtioGpu, "VirtioGPU", "2D only")
        .item(
            RenderBackend::Cpu,
            "CPU",
            "software rendering, no GPU acceleration",
        );

    Ok(prompt.initial_value(default).interact()?)
}

fn ask_gpu_memory(prefilled: Option<u64>) -> Result<u64, WizardError> {
    let mib: u64 = cliclack::input("GPU memory (MiB)\nhost RAM the virtual GPU may use")
        .default_input(&prefilled.unwrap_or(4096).to_string())
        .validate(validate_range(MIN_GPU_MEMORY_MIB, MAX_GPU_MEMORY_MIB))
        .interact()?;
    Ok(mib)
}

/// One validator for the three numeric questions: same wording, same bounds
/// check, and the operator reads the range before anything is parsed.
fn validate_range(min: u64, max: u64) -> impl Fn(&String) -> Result<(), String> {
    move |input: &String| {
        let message = format!("enter an integer between {min} and {max}");
        let value: u64 = input.trim().parse().map_err(|_| message.clone())?;
        if (min..=max).contains(&value) {
            Ok(())
        } else {
            Err(message)
        }
    }
}

fn ask_display_resolution(prefilled: Option<Resolution>) -> Result<Resolution, WizardError> {
    let default = prefilled
        .map(|r| format!("{}x{}", r.width, r.height))
        .unwrap_or_else(|| "1920x1080".to_string());

    let raw: String = cliclack::input(
        "display resolution\napplied by the guest session at boot\n\
         any WxH the virtual display advertises",
    )
    .default_input(&default)
    .validate(|input: &String| parse_resolution(input).map(|_| ()))
    .interact()?;

    parse_resolution(&raw).map_err(WizardError::Message)
}

fn ask_fullscreen(prefilled: Option<bool>) -> Result<bool, WizardError> {
    let fullscreen = cliclack::confirm("start the VM window in fullscreen mode?")
        .initial_value(prefilled.unwrap_or(false))
        .interact()?;
    Ok(fullscreen)
}

fn ask_audio_backend(
    detected: &HardwareDefaults,
    prefilled: Option<AudioBackend>,
) -> Result<AudioBackend, WizardError> {
    let default = prefilled.unwrap_or(detected.audio_server);

    let backend = cliclack::select(format!(
        "audio backend (detected: {})",
        ui::audio_label(detected.audio_server)
    ))
    .item(AudioBackend::Pipewire, "PipeWire", "modern, recommended")
    .item(AudioBackend::Pulseaudio, "PulseAudio", "legacy")
    .item(AudioBackend::None, "None", "no audio")
    .initial_value(default)
    .interact()?;

    Ok(backend)
}

/// Clipboard sharing is a pair: QEMU wires the SPICE agent channel, and the
/// guest needs `spice-vdagent` for it to actually work. The wizard applies
/// the guest half itself (see `wizard::apply`), so the answer here is the
/// whole setting, not half of one.
fn ask_clipboard_enabled(prefilled: Option<bool>) -> Result<bool, WizardError> {
    let clipboard = cliclack::confirm(
        "share the clipboard between host and VM?\ncopy-paste both ways; the guest side needs spice-vdagent,\n\
         which the wizard installs right after creating the VM",
    )
    .initial_value(prefilled.unwrap_or(true))
    .interact()?;
    Ok(clipboard)
}

fn ask_input_pointer(prefilled: Option<PointerMode>) -> Result<PointerMode, WizardError> {
    let default = prefilled.unwrap_or(PointerMode::Tablet);

    let pointer = cliclack::select("input pointer")
        .item(
            PointerMode::Tablet,
            "tablet",
            "absolute coordinates — recommended",
        )
        .item(PointerMode::Mouse, "mouse", "relative coordinates")
        .initial_value(default)
        .interact()?;

    Ok(pointer)
}

fn ask_cpu_cores(prefilled: Option<u32>) -> Result<u32, WizardError> {
    let cores: u32 = cliclack::input("CPU cores\nvirtual CPUs — 4 suits most workloads")
        .default_input(&prefilled.unwrap_or(4).to_string())
        .validate(validate_range(MIN_CPU_CORES.into(), MAX_CPU_CORES.into()))
        .interact()?;
    Ok(cores)
}

fn ask_memory_gib(prefilled: Option<u64>) -> Result<u64, WizardError> {
    let gib: u64 = cliclack::input("memory (GiB)\nguest RAM — 8 GiB suits most workloads")
        .default_input(&prefilled.unwrap_or(8).to_string())
        .validate(validate_range(MIN_MEMORY_GIB, MAX_MEMORY_GIB))
        .interact()?;
    Ok(gib)
}

fn ask_arm_translator(
    detected: &HardwareDefaults,
    prefilled: Option<CliArmTranslator>,
) -> Result<CliArmTranslator, WizardError> {
    let default = prefilled
        .or_else(|| detected.arm_translator.map(CliArmTranslator::from))
        .unwrap_or(CliArmTranslator::None);

    let translator = cliclack::select(format!(
        "ARM translator (detected: {})\n~18 MiB, installed into the instance disk after creation",
        ui::arm_label(detected.arm_translator)
    ))
    .item(CliArmTranslator::Libndk, "libndk", "AMD CPUs")
    .item(CliArmTranslator::Libhoudini, "libhoudini", "Intel CPUs")
    .item(CliArmTranslator::None, "none", "no ARM app support")
    .initial_value(default)
    .interact()?;

    Ok(translator)
}

pub(super) fn ask_gapps(prefilled: Option<bool>) -> Result<bool, WizardError> {
    let gapps = cliclack::confirm(
        "enable GApps?\nGoogle Play Store and services; picks the GAPPS base image\nwhen one exists",
    )
    .initial_value(prefilled.unwrap_or(false))
    .interact()?;
    Ok(gapps)
}

fn ask_linked_overlay(prefilled: Option<bool>) -> Result<bool, WizardError> {
    let linked = cliclack::confirm(
        "link the disk to the base image instead of copying it?\nNo copies the image: independent and safe, uses disk space\n\
         Yes keeps a thin overlay: saves space, but the instance\nbreaks if the base image moves or is deleted",
    )
    .initial_value(prefilled.unwrap_or(false))
    .interact()?;
    Ok(linked)
}

pub fn parse_resolution(s: &str) -> Result<Resolution, String> {
    let s = s.trim();
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| "Invalid format. Use WIDTHxHEIGHT, e.g. 1920x1080".to_string())?;
    let width: u32 = w.trim().parse().map_err(|_| "Invalid width".to_string())?;
    let height: u32 = h.trim().parse().map_err(|_| "Invalid height".to_string())?;
    if !(MIN_RESOLUTION..=MAX_RESOLUTION).contains(&width)
        || !(MIN_RESOLUTION..=MAX_RESOLUTION).contains(&height)
    {
        return Err(format!(
            "Width and height must be between {MIN_RESOLUTION} and {MAX_RESOLUTION}"
        ));
    }
    Ok(Resolution::new(width, height))
}

/// The three answers this question can give, as values instead of label
/// strings: `NetworkMode::Bridge` carries the interface name, which the next
/// question fills in, so the choice itself is a separate type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NetworkChoice {
    Nat,
    Bridge,
    Isolated,
}

impl NetworkChoice {
    fn label(self) -> &'static str {
        match self {
            NetworkChoice::Nat => "NAT",
            NetworkChoice::Bridge => "Bridge",
            NetworkChoice::Isolated => "Isolated",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            NetworkChoice::Nat => "no host setup — the guest shares the host's network",
            NetworkChoice::Bridge => "the guest joins a bridge that already exists on the host",
            NetworkChoice::Isolated => "a private namespace: no route off the host",
        }
    }

    /// Which entry the cursor starts on, so the modify pass opens on what was
    /// answered last time. A bridge interface name is ignored: the entry
    /// stands for the mode, and the name is asked for separately.
    fn from_mode(mode: Option<&NetworkMode>) -> Self {
        match mode {
            Some(NetworkMode::Bridge { .. }) => NetworkChoice::Bridge,
            Some(NetworkMode::Isolated) => NetworkChoice::Isolated,
            _ => NetworkChoice::Nat,
        }
    }
}

fn network_mode(choice: NetworkChoice) -> NetworkMode {
    match choice {
        NetworkChoice::Nat => NetworkMode::Nat,
        NetworkChoice::Bridge => NetworkMode::Bridge {
            interface: String::new(),
        },
        NetworkChoice::Isolated => NetworkMode::Isolated,
    }
}

fn ask_network_mode(prefilled: Option<NetworkMode>) -> Result<NetworkMode, WizardError> {
    let choice = cliclack::select("network mode")
        .item(
            NetworkChoice::Nat,
            NetworkChoice::Nat.label(),
            NetworkChoice::Nat.hint(),
        )
        .item(
            NetworkChoice::Bridge,
            NetworkChoice::Bridge.label(),
            NetworkChoice::Bridge.hint(),
        )
        .item(
            NetworkChoice::Isolated,
            NetworkChoice::Isolated.label(),
            NetworkChoice::Isolated.hint(),
        )
        .initial_value(NetworkChoice::from_mode(prefilled.as_ref()))
        .interact()?;

    Ok(network_mode(choice))
}

fn ask_bridge_interface(prefilled: Option<String>) -> Result<Option<String>, WizardError> {
    let prompt = cliclack::input("bridge interface (e.g. br0)")
        .placeholder("br0")
        .validate(|input: &String| -> Result<(), String> {
            let name = input.trim();
            if name.is_empty() {
                return Err("the interface name cannot be empty".into());
            }
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err("letters, digits, '-' and '_' only".into());
            }
            Ok(())
        });
    let mut prompt = match prefilled {
        Some(name) => prompt.default_input(&name),
        None => prompt,
    };

    let name: String = prompt.interact()?;
    Ok(Some(name.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_numeric_validator_holds_its_bounds() {
        let gpu_memory = validate_range(MIN_GPU_MEMORY_MIB, MAX_GPU_MEMORY_MIB);

        assert!(gpu_memory(&"256".to_string()).is_ok());
        assert!(gpu_memory(&"4096".to_string()).is_ok());
        assert!(gpu_memory(&"16384".to_string()).is_ok());
        assert!(gpu_memory(&"255".to_string()).is_err());
        assert!(gpu_memory(&"16385".to_string()).is_err());
        assert!(gpu_memory(&"lots".to_string()).is_err());
        assert_eq!(
            gpu_memory(&"2048".to_string()).map(|_| "accepted"),
            Ok("accepted"),
            "a value inside the range must pass, not merely be reported on"
        );
    }

    #[test]
    fn the_network_choice_opens_on_the_mode_that_was_already_answered() {
        assert_eq!(
            NetworkChoice::from_mode(Some(&NetworkMode::Bridge {
                interface: "br0".to_string()
            })),
            NetworkChoice::Bridge,
            "the bridge interface name is asked for separately, the entry stands for the mode"
        );
        assert_eq!(
            NetworkChoice::from_mode(Some(&NetworkMode::Isolated)),
            NetworkChoice::Isolated
        );
        assert_eq!(
            NetworkChoice::from_mode(Some(&NetworkMode::Nat)),
            NetworkChoice::Nat
        );
        assert_eq!(NetworkChoice::from_mode(None), NetworkChoice::Nat);
    }

    #[test]
    fn a_bridge_choice_leaves_the_interface_for_the_next_question() {
        assert_eq!(
            network_mode(NetworkChoice::Bridge),
            NetworkMode::Bridge {
                interface: String::new()
            },
            "the interface name comes from its own question, never from the mode choice"
        );
        assert_eq!(network_mode(NetworkChoice::Nat), NetworkMode::Nat);
        assert_eq!(network_mode(NetworkChoice::Isolated), NetworkMode::Isolated);
    }

    #[test]
    fn default_configuration_is_the_recommended_one() {
        let config = AdvancedConfig::default();

        assert_eq!(config.cpu_cores, 4);
        assert_eq!(config.memory_gib, 8);
        assert!(config.clipboard_enabled);
        assert_eq!(config.arm_translator, None);
        assert!(matches!(config.network_mode, NetworkMode::Nat));
    }

    #[test]
    fn group_labels_are_unique() {
        let groups = [
            Group::BootDisks,
            Group::DisplayGpu,
            Group::Devices,
            Group::CpuMemory,
            Group::Network,
            Group::Android,
        ];
        let mut labels: Vec<&str> = groups.iter().map(|g| g.label()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), groups.len());
    }
}
