//! Interactive VM creation wizard (`inquire` prompts).
//!
//! Three creation paths (all coexist):
//! - TOML: `andler create --file vm.toml`
//! - CLI flags: `andler create --kind linux --name ...`
//! - Wizard: `andler create` (this module)

mod advanced;
mod basic;
mod summary;

use std::path::PathBuf;

use andler_core::{
    ArmTranslator, DisplayEngine, RenderBackend,
};
use andler_firmware::{FirmwareError, HardwareDefaults};
use andler_rpc::proto::{
    AndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use inquire::{InquireError, Select};

use crate::helpers::ensure_qcow2_extension;
use crate::{CliAndroidVersion, CliArmTranslator, CliRootMode};

pub use basic::{BasicResult, LinuxBasicResult, AndroidBasicResult};
pub use advanced::AdvancedConfig;
pub use summary::SummaryAction;

/// Result passed back to `create.rs`.
#[derive(Debug)]
#[allow(dead_code)]
pub enum WizardResult {
    Linux(CreateInstanceRequest, String /* instances_root */),
    Android(CreateAndroidInstanceRequest),
}

/// CLI flags already provided before the wizard starts.
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

/// Main wizard entry point.
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

    let mode = ask_wizard_mode()?;
    let kind = basic::ask_kind(partial.kind)?;
    let name = basic::ask_name(partial.name)?;

    let basic_result = match kind {
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

            if detected.ovmf.is_err() {
                eprintln!(
                    "⚠  OVMF not found. Legacy BIOS will be used. \
                     Install edk2-ovmf for UEFI support."
                );
            }

            let basic = LinuxBasicResult {
                name,
                iso_path: iso,
                disk_size_gib: 256,
                instances_root,
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
            let base_image = partial.base_image_path.ok_or_else(|| {
                WizardError::Inquire(
                    "Base image not found: (not specified). Android requires a valid base image."
                        .into(),
                )
            })?;
            if !std::path::Path::new(&base_image).exists() {
                return Err(WizardError::Inquire(format!(
                    "Base image not found: {base_image}. Android requires a valid base image."
                )));
            }

            let instances_root = partial
                .instances_root
                .unwrap_or_else(default_instances_root);

            let basic = AndroidBasicResult {
                name,
                base_image,
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
    let network = build_network_config(detected);
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

    let (gapps, microg, root_mode, magisk_dir) = if let Some(adv) = advanced {
        let (root, dir) = adv
            .root_mode
            .clone()
            .unwrap_or((CliRootMode::None, String::new()));
        (adv.gapps, adv.microg, root, dir)
    } else {
        (false, false, CliRootMode::None, String::new())
    };

    let mut profile = AndroidProfile {
        gapps,
        microg,
        ..Default::default()
    };
    profile.set_android_version(basic.android_version.into());
    profile.set_root(root_mode.into());
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
        magisk_dir,
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

fn build_network_config(detected: &HardwareDefaults) -> andler_core::NetworkConfig {
    let mut network = andler_core::NetworkConfig::reference_default();
    network.nat_backend = summary::format_nat_backend(detected.passt_available);
    network
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

/// Entry point for `andler wizard` and bare `andler` (no subcommand).
/// Runs the interactive wizard with empty PartialArgs, then sends the
/// result to andlerd via gRPC.
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

/// Send wizard result to andlerd via gRPC. Shared between `create::handle()`
/// and `handle_wizard()` to avoid duplicating the gRPC-sending logic.
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
    use std::os::unix::io::AsRawFd;
    libc_isatty(std::io::stdin().as_raw_fd())
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
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
            root_mode: None,
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
            android_version: CliAndroidVersion::Android13,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        };
        let req = build_android_request(&basic, None, &sample_detected()).unwrap();
        assert_eq!(req.name, "android");
        let profile = req.profile.expect("profile");
        assert_eq!(profile.arm_translator(), ProtoArmTranslator::Libndk);
    }

    use andler_rpc::proto::ArmTranslator as ProtoArmTranslator;

    fn sample_detected_no_ovmf() -> HardwareDefaults {
        HardwareDefaults {
            ovmf: Err(FirmwareError::OvmfVarsNotFound),
            ..sample_detected()
        }
    }

    // --- --quick (build_quick is private, so these live inside this module
    // rather than as separate integration tests; see WIZARD.md test plan
    // note on inquire's prompt_with_backend being pub(crate)-only, which
    // rules out simulating the interactive Select/Text prompts below). ---

    #[test]
    fn test_quick_linux_ovmf_not_found() {
        // Linux + --quick + no OVMF -> Legacy BIOS warning on stderr, but
        // the request still succeeds (Linux never requires UEFI).
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
        // Android + --quick + no OVMF -> hard error (Android requires UEFI,
        // no Legacy BIOS fallback).
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
        // Android + --quick + base image path that doesn't exist on disk
        // -> error, even though OVMF is fine.
        let partial = PartialArgs {
            kind: Some(WizardKind::Android),
            name: Some("quick-android-test".into()),
            base_image_path: Some("/nonexistent/base-image.qcow2".into()),
            quick: true,
            instances_root: Some("/tmp/instances".into()),
            ..Default::default()
        };
        let result = build_quick(partial, &sample_detected());
        match result {
            Err(WizardError::Inquire(msg)) => {
                assert!(msg.contains("Base image not found"));
            }
            other => panic!("expected base-image-not-found error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_wizard_not_tty() {
        // Calls the real public `run()` entry point (not a fake/mock): the
        // cargo test harness does not attach a TTY to stdin, so `is_tty()`
        // is false here for real, and this exercises the actual NotTty
        // early-return path without any --quick/--file shortcut.
        let partial = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("interactive-test".into()),
            quick: false,
            ..Default::default()
        };
        let result = run(partial).await;
        assert!(matches!(result, Err(WizardError::NotTty)));
    }
}
