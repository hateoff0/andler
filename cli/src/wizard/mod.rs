

mod advanced;
mod basic;
mod summary;

use std::path::PathBuf;

use andler_core::{
    ArmTranslator, AudioBackend, DisplayEngine, NetworkMode, RenderBackend,
};
use andler_firmware::{FirmwareError, HardwareDefaults};
use andler_rpc::proto::{
    AndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use inquire::{InquireError, Select};

use crate::helpers::ensure_qcow2_extension;
use crate::{CliAndroidVersion, CliArmTranslator};

pub use basic::{BasicResult, LinuxBasicResult, AndroidBasicResult};
pub use advanced::AdvancedConfig;
pub use summary::SummaryAction;


#[derive(Debug)]
#[allow(dead_code)] // variants consumed by caller; Rust can't see cross-module call sites
pub enum WizardResult {
    Linux(CreateInstanceRequest, String /* instances_root */),
    Android(CreateAndroidInstanceRequest),
}


#[derive(Default)]
pub struct PartialArgs {
    pub kind: Option<WizardKind>,
    pub name: Option<String>,
    pub iso_path: Option<String>,
    pub base_image_path: Option<String>,
    pub instances_root: Option<String>,
    pub quick: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WizardKind {
    Linux,
    Android,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WizardMode {
    Basic,
    Advanced,
}


pub async fn run(partial: PartialArgs) -> Result<WizardResult, WizardError> {
    let detected = andler_firmware::detect_all();

    if partial.quick {
        return build_quick(partial, &detected);
    }

    if !is_tty() {
        return Err(WizardError::NotTty);
    }

    if partial.kind == Some(WizardKind::Android) && detected.ovmf.is_err() {
        return Err(WizardError::Firmware(
            detected.ovmf.err().unwrap_or(FirmwareError::OvmfVarsNotFound),
        ));
    }

    print_hardware_summary(&detected, partial.kind);

    let mode = ask_wizard_mode()?;
    let kind = basic::ask_kind(partial.kind)?;
    let name = basic::ask_name(partial.name)?;

    let mut basic_result = match kind {
        WizardKind::Linux => BasicResult::Linux(basic::run_linux(
            name,
            partial.iso_path,
            partial.instances_root,
        )?),
        WizardKind::Android => BasicResult::Android(basic::run_android(
            name,
            partial.base_image_path,
            partial.instances_root,
        )?),
    };

    let mut advanced_config = match mode {
        WizardMode::Advanced => Some(match &basic_result {
            BasicResult::Linux(l) => advanced::run_linux(l, &detected, None)?,
            BasicResult::Android(a) => advanced::run_android(a, &detected, None)?,
        }),
        WizardMode::Basic => None,
    };
    reresolve_android_base_image(&mut basic_result, advanced_config.as_ref(), &detected);

    loop {
        match summary::run(&basic_result, advanced_config.as_ref(), &detected)? {
            SummaryAction::Create => break,
            SummaryAction::Modify => {
                advanced_config = Some(match &basic_result {
                    BasicResult::Linux(l) => {
                        advanced::run_linux(l, &detected, advanced_config.as_ref())?
                    }
                    BasicResult::Android(a) => {
                        advanced::run_android(a, &detected, advanced_config.as_ref())?
                    }
                });
                reresolve_android_base_image(&mut basic_result, advanced_config.as_ref(), &detected);
            }
            SummaryAction::Cancel => return Err(WizardError::Cancelled),
        }
    }

    match &basic_result {
        BasicResult::Linux(l) => {
            if detected.ovmf.is_err() {
                eprintln!(
                    "⚠  OVMF not found. Legacy BIOS will be used. \
                     Install edk2-ovmf for UEFI support."
                );
            }
            let (req, root) = build_linux_request(l, advanced_config.as_ref(), &detected)?;
            Ok(WizardResult::Linux(req, root))
        }
        BasicResult::Android(a) => {
            let req = build_android_request(a, advanced_config.as_ref(), &detected)?;
            Ok(WizardResult::Android(req))
        }
    }
}


fn reresolve_android_base_image(
    basic_result: &mut BasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) {
    let BasicResult::Android(a) = basic_result else {
        return;
    };
    let Some(adv) = advanced else {
        return;
    };
    if !a.base_image_auto_resolved {
        return;
    }

    let profile = andler_core::AndroidProfile {
        android_version: a.android_version.into(),
        gapps: adv.gapps,
        microg: adv.microg,
        arm_translator: resolve_arm_translator(Some(adv), detected).into(),
    };
    match andler_core::base_image::resolve(&profile) {
        Ok(path) => a.base_image = path.to_string_lossy().into_owned(),
        Err(e) => eprintln!(
            "⚠  No base image matches the current Android settings ({e}). \
             Keeping the previous one — pick a different one manually if needed."
        ),
    }
}

fn print_hardware_summary(detected: &HardwareDefaults, kind: Option<WizardKind>) {
    println!("Hardware detected:");

    let render = match detected.gpu_render {
        RenderBackend::Venus => "Venus (Vulkan 3D)",
        RenderBackend::VirGl => "VirGL (OpenGL 3D)",
        RenderBackend::VirtioGpu => "VirtioGPU (2D only)",
        RenderBackend::Cpu => "CPU (software rendering)",
        RenderBackend::Passthrough { .. } => "CPU (software rendering)",
    };
    println!("  GPU render: {render}");

    let display = match detected.display_engine {
        DisplayEngine::Sdl => "SDL",
        DisplayEngine::Gtk => "GTK",
        DisplayEngine::Spice => "SPICE",
        DisplayEngine::Dbus => "D-Bus",
        DisplayEngine::None => "None (headless)",
    };
    println!("  Display:    {display}");

    let audio = match detected.audio_server {
        AudioBackend::Pipewire => "PipeWire",
        AudioBackend::Pulseaudio => "PulseAudio",
        AudioBackend::None => "None",
    };
    println!("  Audio:      {audio}");

    if kind != Some(WizardKind::Linux) {
        let arm = match detected.arm_translator {
            Some(ArmTranslator::Libndk) => "libndk (AMD CPU)",
            Some(ArmTranslator::Libhoudini) => "libhoudini (Intel CPU)",
            Some(ArmTranslator::None) | None => "none",
        };
        println!("  ARM:        {arm}");
    }

    match &detected.ovmf {
        Ok(ovmf) => println!("  OVMF:       {}", ovmf.code.display()),
        Err(err) => println!("  OVMF:       not found ({err})"),
    }

    println!();
}

fn ask_wizard_mode() -> Result<WizardMode, WizardError> {
    let choice = Select::new(
        "Configuration mode:",
        vec![
            "Use recommended settings (Basic)",
            "Customize all settings (Advanced)",
        ],
    )
    .with_help_message(
        "Basic — only essential questions (type, name, ISO, disk size).\n\
         Advanced — full control over GPU, display, audio, CPU, memory, and more.",
    )
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("Use recommended") {
        WizardMode::Basic
    } else {
        WizardMode::Advanced
    })
}

fn build_quick(
    partial: PartialArgs,
    detected: &HardwareDefaults,
) -> Result<WizardResult, WizardError> {
    let kind = partial
        .kind
        .ok_or_else(|| WizardError::Inquire("`--quick` requires `--kind` to specify VM type".into()))?;

    match kind {
        WizardKind::Linux => {
            let name = partial
                .name
                .unwrap_or_else(|| "quick-linux".to_string());
            let iso = partial.iso_path.unwrap_or_default();
            let instances_root = partial
                .instances_root
                .unwrap_or_else(default_instances_root);

            let enable_uefi = if detected.ovmf.is_err() {
                eprintln!(
                    "⚠  OVMF not found. Legacy BIOS will be used. \
                     Install edk2-ovmf for UEFI support."
                );
                false
            } else {
                true
            };

            let basic = LinuxBasicResult {
                name,
                iso_path: iso,
                disk_size_gib: 256,
                instances_root,
                enable_uefi,
            };
            let (req, root) = build_linux_request(&basic, None, detected)?;
            Ok(WizardResult::Linux(req, root))
        }
        WizardKind::Android => {
            if detected.ovmf.is_err() {
                return Err(WizardError::Firmware(FirmwareError::OvmfVarsNotFound));
            }

            let name = partial
                .name
                .unwrap_or_else(|| "quick-android".to_string());
            let base_image_auto_resolved = partial.base_image_path.is_none();
            let base_image = match partial.base_image_path {
                Some(path) => {
                    if !std::path::Path::new(&path).exists() {
                        return Err(WizardError::Inquire(format!(
                            "Base image not found: {path}. Android requires a valid base image."
                        )));
                    }
                    path
                }
                None => {
                    let quick_profile = andler_core::AndroidProfile {
                        android_version: andler_core::AndroidVersion::Android13,
                        gapps: false,
                        microg: false,
                        arm_translator: ArmTranslator::None,
                    };
                    andler_core::base_image::resolve(&quick_profile)
                        .map_err(|e| WizardError::Inquire(e.to_string()))?
                        .to_string_lossy()
                        .into_owned()
                }
            };

            let instances_root = partial
                .instances_root
                .unwrap_or_else(default_instances_root);

            let basic = AndroidBasicResult {
                name,
                base_image,
                base_image_auto_resolved,
                android_version: CliAndroidVersion::Android13,
                disk_size_gib: 256,
                instances_root,
            };
            let req = build_android_request(&basic, None, detected)?;
            Ok(WizardResult::Android(req))
        }
    }
}

pub(crate) fn build_linux_request(
    basic: &LinuxBasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> Result<(CreateInstanceRequest, String), WizardError> {
    let disk_name = format!("{}-disk", basic.name);
    let disk_path = ensure_qcow2_extension(&PathBuf::from(&disk_name));
    let full_disk_path = PathBuf::from(&basic.instances_root).join(&disk_path);

    let mut disk =
        andler_core::DiskConfig::reference_default(full_disk_path);
    disk.size_bytes = basic
        .disk_size_gib
        .checked_mul(andler_core::DiskConfig::GIB)
        .ok_or_else(|| WizardError::Inquire("disk size overflow".into()))?;
    disk.compact_on_shutdown = advanced.map(|a| a.compact_on_shutdown).unwrap_or(false);

    let cdrom_bus = if basic.iso_path.is_empty() {
        andler_core::CdromBus::Ide
    } else {
        advanced
            .and_then(|a| a.cdrom_bus)
            .unwrap_or_else(|| {
                andler_core::CdromBus::recommended_for_iso_filename(std::path::Path::new(
                    &basic.iso_path,
                ))
            })
    };

    let gpu = build_gpu_config(advanced, detected);
    let display = build_display_config(advanced, detected, gpu.render_backend.clone());
    let audio = build_audio_config(advanced, detected);
    let network = build_network_config(advanced, detected)?;
    let input = build_input_config(advanced);
    let cpu = build_cpu_config(advanced);
    let memory = build_memory_config(advanced);

    let ovmf_vars_template = ovmf_vars_template(detected);

    let mut req = CreateInstanceRequest {
        name: basic.name.clone(),
        iso_path: basic.iso_path.clone(),
        cpu: Some(cpu.into()),
        memory: Some(memory.into()),
        disk: Some(disk.into()),
        display: Some(display.into()),
        gpu: Some(gpu.into()),
        network: Some(network.into()),
        firmware: Some(
            andler_core::FirmwareConfig {
                enable_uefi: basic.enable_uefi,
                ovmf_code_path: PathBuf::new(),
                ovmf_vars_path: PathBuf::from(&ovmf_vars_template),
            }
            .into(),
        ),
        audio: Some(audio.into()),
        input: Some(input.into()),
        ..Default::default()
    };
    req.set_cdrom_bus(cdrom_bus.into());

    Ok((req, basic.instances_root.clone()))
}

pub(crate) fn build_android_request(
    basic: &AndroidBasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> Result<CreateAndroidInstanceRequest, WizardError> {
    let arm_translator = resolve_arm_translator(advanced, detected);

    let (gapps, microg) = if let Some(adv) = advanced {
        (adv.gapps, adv.microg)
    } else {
        (false, false)
    };

    let mut profile = AndroidProfile {
        gapps,
        microg,
        ..Default::default()
    };
    profile.set_android_version(basic.android_version.into());
    profile.set_arm_translator(arm_translator.into());

    Ok(CreateAndroidInstanceRequest {
        name: basic.name.clone(),
        profile: Some(profile),
        base_image_path: basic.base_image.clone(),
        instances_root: basic.instances_root.clone(),
        overlay_size_bytes: basic
            .disk_size_gib
            .checked_mul(andler_core::DiskConfig::GIB)
            .ok_or_else(|| WizardError::Inquire("overlay size overflow".into()))?,
        ovmf_vars_template: ovmf_vars_template(detected),
        linked_overlay: advanced.map(|a| a.linked_overlay).unwrap_or(false),
    })
}

fn build_gpu_config(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> andler_core::GpuConfig {
    let mut gpu = andler_core::GpuConfig::reference_default();
    gpu.render_backend = advanced
        .map(|a| a.gpu_render.clone())
        .unwrap_or_else(|| detected.gpu_render.clone());
    gpu.hostmem_bytes = advanced
        .map(|a| a.gpu_memory_mib * andler_core::GpuConfig::MIB)
        .unwrap_or(gpu.hostmem_bytes);
    if gpu.render_backend == RenderBackend::Cpu {
        gpu.blob = false;
        gpu.gl = false;
    }
    gpu
}

fn build_display_config(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
    render: RenderBackend,
) -> andler_core::DisplayConfig {
    let mut display = andler_core::DisplayConfig::reference_default();
    if let Some(adv) = advanced {
        display.resolution = adv.display_resolution;
        display.fullscreen = adv.fullscreen;
    }
    display.display_engine = if render == RenderBackend::Cpu {
        DisplayEngine::None
    } else {
        detected.display_engine
    };
    display
}

fn build_audio_config(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> andler_core::AudioConfig {
    let mut audio = andler_core::AudioConfig::reference_default();
    audio.backend = advanced
        .map(|a| a.audio_backend)
        .unwrap_or(detected.audio_server);
    audio
}

fn build_network_config(advanced: Option<&AdvancedConfig>, detected: &HardwareDefaults) -> Result<andler_core::NetworkConfig, WizardError> {
    let mut network = andler_core::NetworkConfig::reference_default();
    
    match advanced.map(|a| a.network_mode.clone()) {
        Some(NetworkMode::Bridge { .. }) => {
            network.mode = NetworkMode::Bridge {
                interface: advanced
                    .unwrap()
                    .bridge_interface
                    .clone()
                    .ok_or(WizardError::InvalidConfig("Bridge mode requires bridge interface".to_string()))?,
            };
        }
        Some(NetworkMode::Isolated) => {
            network.mode = NetworkMode::Isolated;
        }
        Some(NetworkMode::Nat) | None => {
            network.nat_backend = summary::format_nat_backend(detected.passt_available);
        }
    }
    
    Ok(network)
}
fn build_input_config(advanced: Option<&AdvancedConfig>) -> andler_core::InputConfig {
    let mut input = andler_core::InputConfig::reference_default();
    if let Some(adv) = advanced {
        input.pointer_mode = adv.input_pointer;
        input.clipboard_enabled = adv.clipboard_enabled;
    }
    input
}

fn build_cpu_config(advanced: Option<&AdvancedConfig>) -> andler_core::CpuConfig {
    let mut cpu = andler_core::CpuConfig::reference_default();
    if let Some(adv) = advanced {
        cpu.cores = adv.cpu_cores;
    }
    cpu
}

fn build_memory_config(advanced: Option<&AdvancedConfig>) -> andler_core::MemoryConfig {
    let mut memory = andler_core::MemoryConfig::reference_default();
    if let Some(adv) = advanced {
        memory.size_bytes = adv
            .memory_gib
            .checked_mul(andler_core::MemoryConfig::GIB)
            .unwrap_or(memory.size_bytes);
    }
    memory
}

fn resolve_arm_translator(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> CliArmTranslator {
    if let Some(adv) = advanced.and_then(|a| a.arm_translator) {
        return adv;
    }
    detected.arm_translator.map(|t| match t {
        ArmTranslator::Libndk => CliArmTranslator::Libndk,
        ArmTranslator::Libhoudini => CliArmTranslator::Libhoudini,
        ArmTranslator::None => CliArmTranslator::None,
    }).unwrap_or(CliArmTranslator::None)
}

fn ovmf_vars_template(detected: &HardwareDefaults) -> String {
    match &detected.ovmf {
        Ok(found) => found.vars_template.to_string_lossy().into_owned(),
        Err(_) => String::new(),
    }
}

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
}


pub async fn handle_wizard(
    client: &mut andler_rpc::proto::andler_service_client::AndlerServiceClient<
        tonic::transport::Channel,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    let partial = PartialArgs::default();
    let result = run(partial).await;
    match result {
        Ok(result) => send_result(client, result).await,
        Err(WizardError::Cancelled) => {
            println!("Cancelled.");
            Ok(())
        }
        Err(WizardError::NotTty) => {
            eprintln!(
                "Interactive wizard is not available (no TTY).\n\
                 Use `andler create --kind linux --name <name> --iso-path <path> --disk-path <path>`\n\
                 or `andler create --file vm.toml`."
            );
            std::process::exit(2);
        }
        Err(e) => Err(e.into()),
    }
}


pub async fn send_result(
    client: &mut andler_rpc::proto::andler_service_client::AndlerServiceClient<
        tonic::transport::Channel,
    >,
    result: WizardResult,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        WizardResult::Linux(req, _) => {
            let id = client.create_instance(req).await?.into_inner().instance_id;
            println!("✓ VM created: {id}");
            println!("  andler start {id}");
        }
        WizardResult::Android(req) => {
            let id = client
                .create_android_instance(req)
                .await?
                .into_inner()
                .instance_id;
            println!("✓ Android VM created: {id}");
            println!("  andler start {id}");
        }
    }
    Ok(())
}

pub(crate) fn is_tty() -> bool {
    if std::env::var("ANDLER_WIZARD_NOT_TTY").is_ok() {
        return false;
    }
    use std::os::unix::io::AsRawFd;
    libc_isatty(std::io::stdin().as_raw_fd())
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    // SAFETY: isatty(2) is a pure POSIX call — takes an fd, returns int, no mutable state.
    unsafe { isatty(fd) != 0 }
}

pub(crate) fn map_inquire_err(e: InquireError) -> WizardError {
    match e {
        InquireError::NotTTY => WizardError::NotTty,
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            WizardError::Cancelled
        }
        other => WizardError::Inquire(other.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error(
        "Interactive wizard is not available (no TTY). \
         Pass parameters via flags or use `--file <config.toml>`.\n\
         Example:\n\
          andler create --kind linux --name <name> --iso-path <path> \
         --disk-path <path>\n\
         Or:\n\
          andler create --file vm.toml"
    )]
    NotTty,

    #[error("wizard cancelled")]
    Cancelled,

    #[error("{0}")]
    Inquire(String),

    #[error("{0}")]
    InvalidConfig(String),

    #[error("{0}")]
    Firmware(#[from] FirmwareError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{AudioBackend, PointerMode, Resolution};
    use andler_firmware::DetectedOvmf;

    fn sample_detected() -> HardwareDefaults {
        HardwareDefaults {
            ovmf: Ok(DetectedOvmf {
                code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
                vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
            }),
            gpu_render: RenderBackend::Venus,
            display_engine: DisplayEngine::Gtk,
            audio_server: AudioBackend::Pipewire,
            arm_translator: Some(ArmTranslator::Libndk),
            venus_supported: true,
            passt_available: true,
        }
    }

    #[test]
    fn build_create_request_linux_basic_mode() {
        let basic = LinuxBasicResult {
            name: "test".into(),
            iso_path: "/tmp/test.iso".into(),
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
            enable_uefi: true,
        };
        let detected = sample_detected();
        let (req, root) = build_linux_request(&basic, None, &detected).unwrap();
        assert_eq!(req.name, "test");
        assert_eq!(root, "/tmp/instances");
        assert_eq!(req.iso_path, "/tmp/test.iso");
    }

    #[test]
    fn build_create_request_linux_advanced_clipboard() {
        let basic = LinuxBasicResult {
            name: "test".into(),
            iso_path: String::new(),
            disk_size_gib: 128,
            instances_root: "/tmp/instances".into(),
            enable_uefi: true,
        };
        let advanced = AdvancedConfig {
            cdrom_bus: None,
            compact_on_shutdown: true,
            gpu_render: RenderBackend::VirGl,
            gpu_memory_mib: 8192,
            display_resolution: Resolution::new(2560, 1440),
            fullscreen: true,
            audio_backend: AudioBackend::None,
            clipboard_enabled: false,
            input_pointer: PointerMode::Mouse,
            cpu_cores: 8,
            memory_gib: 16,
            arm_translator: None,
            gapps: false,
            microg: false,
            network_mode: NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        };
        let (req, _) = build_linux_request(&basic, Some(&advanced), &sample_detected()).unwrap();
        let input = req.input.expect("input");
        assert!(!input.clipboard_enabled);
    }

    #[test]
    fn build_create_request_android_basic_mode() {
        let basic = AndroidBasicResult {
            name: "android".into(),
            base_image: "/tmp/base.qcow2".into(),
            base_image_auto_resolved: false,
            android_version: CliAndroidVersion::Android13,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        };
        let req = build_android_request(&basic, None, &sample_detected()).unwrap();
        assert_eq!(req.name, "android");
        let profile = req.profile.expect("profile");
        assert_eq!(profile.arm_translator(), ProtoArmTranslator::Libndk);
    }

    static ANDLER_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct AndlerHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        base: PathBuf,
    }

    impl AndlerHomeGuard {
        fn new() -> (Self, PathBuf) {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let lock = ANDLER_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "andler-wizard-test-home-{}-{n}",
                std::process::id()
            ));
            let cache_dir = base.join("cache").join("base-images");
            std::fs::create_dir_all(&cache_dir).unwrap();
            std::env::set_var(andler_core::paths::ANDLER_HOME_ENV, &base);
            (Self { _lock: lock, base }, cache_dir)
        }
    }

    impl Drop for AndlerHomeGuard {
        fn drop(&mut self) {
            std::env::remove_var(andler_core::paths::ANDLER_HOME_ENV);
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn write_manifest(dir: &std::path::Path, name: &str, major: &str, variant: &str) {
        std::fs::write(dir.join(format!("{name}.qcow2")), b"placeholder").unwrap();
        std::fs::write(
            dir.join(format!("{name}.manifest.json")),
            format!(
                r#"{{"schema_version":1,"android_major":"{major}","android_variant":"{variant}","built_at":"2026-01-01T00:00:00Z"}}"#
            ),
        )
        .unwrap();
    }

    fn sample_advanced(gapps: bool) -> AdvancedConfig {
        AdvancedConfig {
            cdrom_bus: None,
            compact_on_shutdown: false,
            gpu_render: RenderBackend::Venus,
            gpu_memory_mib: 1024,
            display_resolution: Resolution::new(1920, 1080),
            fullscreen: false,
            audio_backend: AudioBackend::None,
            clipboard_enabled: true,
            input_pointer: PointerMode::Mouse,
            cpu_cores: 4,
            memory_gib: 8,
            arm_translator: None,
            gapps,
            microg: false,
            network_mode: NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        }
    }

    #[test]
    fn reresolve_android_base_image_switches_to_gapps_variant() {
        let (_guard, dir) = AndlerHomeGuard::new();
        write_manifest(&dir, "vanilla", "13", "VANILLA");
        write_manifest(&dir, "gapps", "13", "GAPPS");

        let mut basic_result = BasicResult::Android(AndroidBasicResult {
            name: "android".into(),
            base_image: dir.join("vanilla.qcow2").to_string_lossy().into_owned(),
            base_image_auto_resolved: true,
            android_version: CliAndroidVersion::Android13,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        });
        let advanced = sample_advanced(true);
        reresolve_android_base_image(&mut basic_result, Some(&advanced), &sample_detected());

        let BasicResult::Android(a) = &basic_result else {
            panic!("expected Android variant");
        };
        assert_eq!(
            a.base_image,
            dir.join("gapps.qcow2").to_string_lossy().into_owned()
        );
    }

    #[test]
    fn reresolve_android_base_image_leaves_manually_entered_path_alone() {
        let (_guard, _dir) = AndlerHomeGuard::new();

        let mut basic_result = BasicResult::Android(AndroidBasicResult {
            name: "android".into(),
            base_image: "/tmp/hand-picked.qcow2".into(),
            base_image_auto_resolved: false,
            android_version: CliAndroidVersion::Android13,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        });
        let advanced = sample_advanced(true);
        reresolve_android_base_image(&mut basic_result, Some(&advanced), &sample_detected());

        let BasicResult::Android(a) = &basic_result else {
            panic!("expected Android variant");
        };
        assert_eq!(a.base_image, "/tmp/hand-picked.qcow2");
    }

    use andler_rpc::proto::ArmTranslator as ProtoArmTranslator;

    fn sample_detected_no_ovmf() -> HardwareDefaults {
        HardwareDefaults {
            ovmf: Err(FirmwareError::OvmfVarsNotFound),
            ..sample_detected()
        }
    }


    #[test]
    fn test_quick_linux_ovmf_not_found() {
        let partial = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("quick-linux-test".into()),
            quick: true,
            instances_root: Some("/tmp/instances".into()),
            ..Default::default()
        };
        let result = build_quick(partial, &sample_detected_no_ovmf());
        assert!(result.is_ok());
        match result.unwrap() {
            WizardResult::Linux(req, _root) => assert_eq!(req.name, "quick-linux-test"),
            WizardResult::Android(_) => panic!("expected Linux result"),
        }
    }

    #[test]
    fn test_quick_android_ovmf_not_found() {
        let partial = PartialArgs {
            kind: Some(WizardKind::Android),
            name: Some("quick-android-test".into()),
            base_image_path: Some("/tmp/base.qcow2".into()),
            quick: true,
            instances_root: Some("/tmp/instances".into()),
            ..Default::default()
        };
        let result = build_quick(partial, &sample_detected_no_ovmf());
        assert!(matches!(
            result,
            Err(WizardError::Firmware(FirmwareError::OvmfVarsNotFound))
        ));
    }

    #[test]
    fn test_quick_android_base_image_not_found() {
        let partial = PartialArgs {
            kind: Some(WizardKind::Android),
            name: Some("quick-android-test".into()),
            base_image_path: Some("/nonexistent/base-image.qcow2".into()),
            quick: true,
            instances_root: Some("/tmp/instances".into()),
            ..Default::default()
        };
        let result = build_quick(partial, &sample_detected());
        assert!(matches!(
            result,
            Err(WizardError::Inquire(msg)) if msg.contains("Base image not found")
        ));
    }
    #[tokio::test]
    async fn test_wizard_not_tty() {
        std::env::set_var("ANDLER_WIZARD_NOT_TTY", "1");
        let partial = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("interactive-test".into()),
            quick: false,
            ..Default::default()
        };
        let result = run(partial).await;
        assert!(matches!(result, Err(WizardError::NotTty)));
    }

    #[test]
    fn test_build_network_config() {
        let detected = sample_detected();
        
        let advanced = AdvancedConfig {
            cdrom_bus: None,
            compact_on_shutdown: false,
            gpu_render: RenderBackend::Cpu,
            gpu_memory_mib: 0,
            display_resolution: Resolution::new(0, 0),
            fullscreen: false,
            audio_backend: AudioBackend::Pulseaudio,
            clipboard_enabled: false,
            input_pointer: PointerMode::Mouse,
            cpu_cores: 1,
            memory_gib: 1,
            arm_translator: None,
            gapps: false,
            microg: false,
            network_mode: NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        };
        let network = build_network_config(Some(&advanced), &detected).unwrap();
        assert!(matches!(network.mode, NetworkMode::Nat));
        
        let advanced_bridge = AdvancedConfig {
            cdrom_bus: None,
            compact_on_shutdown: false,
            gpu_render: RenderBackend::Cpu,
            gpu_memory_mib: 0,
            display_resolution: Resolution::new(0, 0),
            fullscreen: false,
            audio_backend: AudioBackend::Pulseaudio,
            clipboard_enabled: false,
            input_pointer: PointerMode::Mouse,
            cpu_cores: 1,
            memory_gib: 1,
            arm_translator: None,
            gapps: false,
            microg: false,
            network_mode: NetworkMode::Bridge { interface: "br0".to_string() },
            bridge_interface: Some("br0".to_string()),
            linked_overlay: false,
        };
        let network = build_network_config(Some(&advanced_bridge), &detected).unwrap();
        assert!(matches!(network.mode, NetworkMode::Bridge { .. }));
        
        let advanced_isolated = AdvancedConfig {
            cdrom_bus: None,
            compact_on_shutdown: false,
            gpu_render: RenderBackend::Cpu,
            gpu_memory_mib: 0,
            display_resolution: Resolution::new(0, 0),
            fullscreen: false,
            audio_backend: AudioBackend::Pulseaudio,
            clipboard_enabled: false,
            input_pointer: PointerMode::Mouse,
            cpu_cores: 1,
            memory_gib: 1,
            arm_translator: None,
            gapps: false,
            microg: false,
            network_mode: NetworkMode::Isolated,
            bridge_interface: None,
            linked_overlay: false,
        };
        let network = build_network_config(Some(&advanced_isolated), &detected).unwrap();
        assert!(matches!(network.mode, NetworkMode::Isolated));
    }
}
