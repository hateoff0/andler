use std::path::{Path, PathBuf};

use andler_core::config::{ConfigDraft, DiskDraft, DiskSource, DraftKind, FirmwareDraft};
use andler_core::{
    AndroidBootMode, AndroidProfile, ArmTranslator, CdromBus, DisplayEngine, NetworkMode,
    RenderBackend,
};
use andler_firmware::HardwareDefaults;

use crate::helpers::ensure_qcow2_extension;
use crate::instance_file::TemplateFile;
use crate::{CliAndroidVersion, CliArmTranslator};

use super::advanced::AdvancedConfig;
use super::basic::{AndroidBasicResult, BasicResult, LinuxBasicResult};
use super::summary::format_nat_backend;
use super::{ui, PartialArgs, WizardError};

/// One section of the Linux draft: an advanced answer wins, then a template's
/// section, then hardware detection. Same order the CLI-flag path applies
/// (template over detection, flags over both), so the wizard cannot resolve a
/// section to anything the flag or file path would not.
fn layered<T>(answer: Option<T>, from_template: Option<T>, detected: impl FnOnce() -> T) -> T {
    match (answer, from_template) {
        (Some(answer), _) => answer,
        (None, Some(from_template)) => from_template,
        (None, None) => detected(),
    }
}

fn core_cdrom_bus(flags: &PartialArgs) -> Option<CdromBus> {
    match flags.cdrom_bus? {
        crate::CliCdromBus::Auto => None,
        crate::CliCdromBus::Virtio => Some(CdromBus::VirtioScsi),
        crate::CliCdromBus::Ide => Some(CdromBus::Ide),
    }
}

fn load_template(flags: &PartialArgs) -> Result<Option<TemplateFile>, WizardError> {
    match flags.template.as_deref() {
        Some(name) => TemplateFile::load(name)
            .map(Some)
            .map_err(|e| WizardError::Inquire(e.to_string())),
        None => Ok(None),
    }
}

/// The wizard's Linux draft — the interactive answers and `--quick`, both
/// filled from the CLI-flag layer and resolved by the one resolver.
pub(crate) fn linux_draft(
    basic: &LinuxBasicResult,
    advanced: Option<&AdvancedConfig>,
    flags: &PartialArgs,
    detected: &HardwareDefaults,
) -> Result<ConfigDraft, WizardError> {
    let template = load_template(flags)?;
    let template = template.as_ref();

    let disk_path = match &flags.disk_path {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(&basic.instances_root)
            .join(ensure_qcow2_extension(&PathBuf::from("disk"))),
    };

    let cdrom_bus = advanced
        .and_then(|a| a.cdrom_bus)
        .or_else(|| core_cdrom_bus(flags))
        .unwrap_or_else(|| {
            if basic.iso_path.is_empty() {
                CdromBus::Ide
            } else {
                CdromBus::recommended_for_iso_filename(Path::new(&basic.iso_path))
            }
        });

    let gpu = layered(
        advanced.map(|a| build_gpu_config(Some(a), detected)),
        template.and_then(|t| t.gpu.clone()),
        || build_gpu_config(None, detected),
    );
    let display = layered(
        advanced.map(|a| build_display_config(Some(a), detected, gpu.render_backend.clone())),
        template.and_then(|t| t.display),
        || build_display_config(None, detected, gpu.render_backend.clone()),
    );
    let network = match (advanced, template.and_then(|t| t.network.clone())) {
        (Some(adv), _) => build_network_config(Some(adv), detected)?,
        (None, Some(from_template)) => from_template,
        (None, None) => build_network_config(None, detected)?,
    };

    Ok(ConfigDraft {
        name: basic.name.clone(),
        kind: DraftKind::Linux {
            iso_path: PathBuf::from(&basic.iso_path),
            cdrom_bus,
        },
        disk: DiskDraft {
            path: disk_path,
            source: DiskSource::Fresh,
            size_gib: Some(basic.disk_size_gib),
            compact_on_shutdown: advanced
                .map(|a| a.compact_on_shutdown)
                .or(flags.compact_on_shutdown)
                .unwrap_or(false),
            snapshot_timeout_secs: None,
        },
        firmware: FirmwareDraft {
            enable_uefi: basic.enable_uefi.or(flags.enable_uefi),
            ovmf_vars_template: flags.ovmf_vars_template.clone().map(PathBuf::from),
        },
        autostart: false,
        cpu: Some(layered(
            advanced.map(|a| build_cpu_config(Some(a))),
            template.and_then(|t| t.cpu.clone()),
            || build_cpu_config(None),
        )),
        memory: Some(layered(
            advanced.map(|a| build_memory_config(Some(a))),
            template.and_then(|t| t.memory.clone()),
            || build_memory_config(None),
        )),
        display: Some(display),
        gpu: Some(gpu),
        network: Some(network),
        audio: Some(layered(
            advanced.map(|a| build_audio_config(Some(a), detected)),
            template.and_then(|t| t.audio),
            || build_audio_config(None, detected),
        )),
        input: Some(layered(
            advanced.map(|a| build_input_config(Some(a))),
            template.and_then(|t| t.input),
            || build_input_config(None),
        )),
    })
}

/// The wizard's Android draft. Android instances take their config sections
/// from the profile, so `--template` is refused for Android by the flag path
/// rather than dropped here.
pub(crate) fn android_draft(
    basic: &AndroidBasicResult,
    advanced: Option<&AdvancedConfig>,
    flags: &PartialArgs,
    detected: &HardwareDefaults,
) -> Result<ConfigDraft, WizardError> {
    let arm_translator = match advanced.and_then(|a| a.arm_translator) {
        Some(answer) => answer,
        None => match flags.arm_translator {
            Some(flag) => flag,
            None => resolve_arm_translator(None, detected),
        },
    };
    let gapps = advanced.map(|a| a.gapps).unwrap_or(basic.gapps);

    let profile = AndroidProfile {
        android_version: basic.android_version.into(),
        gapps,
        microg: flags.microg,
        arm_translator: arm_translator.into(),
        boot_mode: AndroidBootMode::Android,
        base_image_pin: None,
    };

    Ok(ConfigDraft {
        name: basic.name.clone(),
        kind: DraftKind::Android { profile },
        disk: DiskDraft {
            path: PathBuf::from(&basic.instances_root).join("disk.qcow2"),
            source: DiskSource::BaseImage {
                path: PathBuf::from(&basic.base_image),
                linked: advanced.map(|a| a.linked_overlay).unwrap_or(flags.linked),
            },
            size_gib: flags.overlay_size_gib,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        },
        firmware: FirmwareDraft {
            enable_uefi: None,
            ovmf_vars_template: flags.ovmf_vars_template.clone().map(PathBuf::from),
        },
        autostart: false,
        cpu: None,
        memory: None,
        display: None,
        gpu: None,
        network: None,
        audio: None,
        input: None,
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

fn build_network_config(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> Result<andler_core::NetworkConfig, WizardError> {
    let mut network = andler_core::NetworkConfig::reference_default();

    match advanced.map(|a| a.network_mode.clone()) {
        Some(NetworkMode::Bridge { .. }) => {
            network.mode = NetworkMode::Bridge {
                interface: advanced.unwrap().bridge_interface.clone().ok_or(
                    WizardError::InvalidConfig("Bridge mode requires bridge interface".to_string()),
                )?,
            };
        }
        Some(NetworkMode::Isolated) => {
            network.mode = NetworkMode::Isolated;
        }
        Some(NetworkMode::Nat) | None => {
            network.nat_backend = format_nat_backend(detected.passt_available);
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

pub(crate) fn resolve_arm_translator(
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> CliArmTranslator {
    if let Some(adv) = advanced.and_then(|a| a.arm_translator) {
        return adv;
    }
    detected
        .arm_translator
        .map(|t| match t {
            ArmTranslator::Libndk => CliArmTranslator::Libndk,
            ArmTranslator::Libhoudini => CliArmTranslator::Libhoudini,
            ArmTranslator::None => CliArmTranslator::None,
        })
        .unwrap_or(CliArmTranslator::None)
}

/// Re-resolves an auto-picked Android base image when the advanced answers
/// changed which image the profile matches (e.g. GApps toggled on). A path
/// the user typed is never touched: the wizard does not second-guess an
/// explicit choice.
pub(crate) fn reresolve_android_base_image(
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
        microg: false,
        arm_translator: resolve_arm_translator(Some(adv), detected).into(),
        boot_mode: AndroidBootMode::Android,
        base_image_pin: None,
    };
    match andler_core::base_image::resolve(&profile) {
        Ok(path) => a.base_image = path.to_string_lossy().into_owned(),
        Err(e) => ui::result(
            ui::Status::Warn,
            &format!(
                "No base image matches the current Android settings ({e}). Keeping the previous \
             one — download a matching build with `andler image download --android-version \
             {} --variant {}` and pick it, or choose a different one manually.",
                a.android_version as u8,
                if adv.gapps { "GAPPS" } else { "VANILLA" },
            ),
        ),
    }
}

/// The domain `AndroidProfile` a set of basic answers describes — what the
/// local base-image cache is matched against (and what the download filter
/// mirrors in the daemon).
pub(crate) fn android_profile_for(
    version: CliAndroidVersion,
    gapps: bool,
    translator: CliArmTranslator,
) -> andler_core::AndroidProfile {
    andler_core::AndroidProfile {
        android_version: version.into(),
        gapps,
        microg: false,
        arm_translator: translator.into(),
        boot_mode: AndroidBootMode::Android,
        base_image_pin: None,
    }
}

#[cfg(test)]
pub(crate) fn sample_detected() -> HardwareDefaults {
    HardwareDefaults {
        ovmf: Ok(andler_firmware::DetectedOvmf {
            code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
        }),
        gpu_render: RenderBackend::Venus,
        display_engine: DisplayEngine::Gtk,
        audio_server: andler_core::AudioBackend::Pipewire,
        arm_translator: Some(ArmTranslator::Libndk),
        venus_supported: true,
        passt_available: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{AudioBackend, PointerMode, Resolution};

    use super::sample_detected;

    fn resolve_linux(
        basic: &LinuxBasicResult,
        advanced: Option<&AdvancedConfig>,
        flags: &PartialArgs,
    ) -> andler_core::InstanceConfig {
        let detected = sample_detected();
        let draft = linux_draft(basic, advanced, flags, &detected).expect("draft must build");
        let host = crate::create::host_firmware(&detected);
        crate::create::Creation::resolve(&draft, "/tmp/instances".to_string(), &host)
            .expect("must resolve")
            .cfg
    }

    #[test]
    fn build_create_request_linux_basic_mode() {
        let basic = LinuxBasicResult {
            name: "test".into(),
            iso_path: "/tmp/test.iso".into(),
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
            enable_uefi: Some(true),
        };
        let cfg = resolve_linux(&basic, None, &PartialArgs::default());
        assert_eq!(cfg.name, "test");
        assert_eq!(
            cfg.kind,
            andler_core::InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: andler_core::CdromBus::Ide,
            }
        );
        assert_eq!(cfg.disk.size_bytes, 256 * andler_core::DiskConfig::GIB);
        assert_eq!(cfg.disk.path, PathBuf::from("/tmp/instances/disk.qcow2"));
        assert!(cfg.firmware.enable_uefi);
        cfg.validate()
            .expect("the wizard must resolve a valid config");
    }

    #[test]
    fn build_create_request_linux_advanced_clipboard() {
        let basic = LinuxBasicResult {
            name: "test".into(),
            iso_path: String::new(),
            disk_size_gib: 128,
            instances_root: "/tmp/instances".into(),
            enable_uefi: Some(true),
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
            network_mode: NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        };
        let cfg = resolve_linux(&basic, Some(&advanced), &PartialArgs::default());
        assert!(!cfg.input.clipboard_enabled);
        assert_eq!(cfg.cpu.cores, 8);
        assert_eq!(cfg.memory.size_bytes, 16 * andler_core::MemoryConfig::GIB);
        assert_eq!(cfg.gpu.render_backend, RenderBackend::VirGl);
        assert_eq!(cfg.gpu.hostmem_bytes, 8192 * andler_core::GpuConfig::MIB);
        assert_eq!(cfg.display.resolution, Resolution::new(2560, 1440));
        assert_eq!(cfg.audio.backend, AudioBackend::None);
        assert!(cfg.disk.compact_on_shutdown);
        cfg.validate()
            .expect("the wizard must resolve a valid config");
    }

    #[test]
    fn a_template_fills_sections_the_basic_flow_never_asks_about() {
        let basic = LinuxBasicResult {
            name: "templated".into(),
            iso_path: String::new(),
            disk_size_gib: 64,
            instances_root: "/tmp/instances".into(),
            enable_uefi: Some(true),
        };
        let flags = PartialArgs {
            template: Some("headless".to_string()),
            ..Default::default()
        };
        let cfg = resolve_linux(&basic, None, &flags);
        assert_eq!(cfg.gpu.render_backend, RenderBackend::Cpu);
        assert_eq!(cfg.display.display_engine, DisplayEngine::None);
        assert_eq!(cfg.audio.backend, andler_core::AudioBackend::None);
        cfg.validate()
            .expect("the wizard must resolve a valid config");
    }

    #[test]
    fn build_create_request_android_basic_mode() {
        let basic = AndroidBasicResult {
            name: "android".into(),
            base_image: "/tmp/base.qcow2".into(),
            base_image_auto_resolved: false,
            android_version: CliAndroidVersion::Android13,
            gapps: false,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        };
        let detected = sample_detected();
        let flags = PartialArgs::default();
        let draft = android_draft(&basic, None, &flags, &detected).expect("draft must build");
        let host = crate::create::host_firmware(&detected);
        let creation =
            crate::create::Creation::resolve(&draft, "/tmp/instances".to_string(), &host)
                .expect("must resolve");

        assert_eq!(creation.cfg.name, "android");
        assert_eq!(
            creation.cfg.disk.size_bytes,
            128 * andler_core::DiskConfig::GIB,
            "overlay size must be the resolver's 128 GiB default, not derived from disk size"
        );
        let andler_core::InstanceKind::AndroidVm { android_profile } = creation.cfg.kind else {
            panic!("expected an Android VM");
        };
        assert_eq!(android_profile.arm_translator, ArmTranslator::Libndk);
    }

    #[test]
    fn android_profile_for_maps_the_cli_answer_to_the_domain_profile() {
        let profile = android_profile_for(
            CliAndroidVersion::Android11,
            true,
            CliArmTranslator::Libhoudini,
        );

        assert_eq!(
            profile.android_version,
            andler_core::AndroidVersion::Android11
        );
        assert_eq!(profile.arm_translator, ArmTranslator::Libhoudini);
        assert!(profile.gapps);
    }

    #[test]
    fn test_build_network_config() {
        let detected = sample_detected();

        let nat = AdvancedConfig {
            network_mode: NetworkMode::Nat,
            ..Default::default()
        };
        assert!(matches!(
            build_network_config(Some(&nat), &detected).unwrap().mode,
            NetworkMode::Nat
        ));

        let bridge = AdvancedConfig {
            network_mode: NetworkMode::Bridge {
                interface: "br0".to_string(),
            },
            bridge_interface: Some("br0".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            build_network_config(Some(&bridge), &detected).unwrap().mode,
            NetworkMode::Bridge { .. }
        ));

        let isolated = AdvancedConfig {
            network_mode: NetworkMode::Isolated,
            ..Default::default()
        };
        assert!(matches!(
            build_network_config(Some(&isolated), &detected)
                .unwrap()
                .mode,
            NetworkMode::Isolated
        ));

        let missing_interface = AdvancedConfig {
            network_mode: NetworkMode::Bridge {
                interface: String::new(),
            },
            bridge_interface: None,
            ..Default::default()
        };
        assert!(matches!(
            build_network_config(Some(&missing_interface), &detected),
            Err(WizardError::InvalidConfig(_))
        ));
    }

    // --- base-image re-resolution ------------------------------------------

    struct AndlerHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        base: PathBuf,
    }

    impl AndlerHomeGuard {
        fn new() -> (Self, PathBuf) {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let lock = super::super::ANDLER_HOME_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "andler-wizard-build-test-home-{}-{n}",
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

    fn android_result(base_image: String, auto_resolved: bool) -> BasicResult {
        BasicResult::Android(AndroidBasicResult {
            name: "android".into(),
            base_image,
            base_image_auto_resolved: auto_resolved,
            android_version: CliAndroidVersion::Android13,
            gapps: false,
            disk_size_gib: 256,
            instances_root: "/tmp/instances".into(),
        })
    }

    fn advanced_with_gapps(gapps: bool) -> AdvancedConfig {
        AdvancedConfig {
            gapps,
            ..Default::default()
        }
    }

    #[test]
    fn reresolve_switches_an_auto_picked_image_to_the_gapps_variant() {
        let (_guard, dir) = AndlerHomeGuard::new();
        write_manifest(&dir, "vanilla", "13", "VANILLA");
        write_manifest(&dir, "gapps", "13", "GAPPS");
        let mut basic_result = android_result(
            dir.join("vanilla.qcow2").to_string_lossy().into_owned(),
            true,
        );

        reresolve_android_base_image(
            &mut basic_result,
            Some(&advanced_with_gapps(true)),
            &sample_detected(),
        );

        let BasicResult::Android(a) = &basic_result else {
            panic!("expected Android");
        };
        assert_eq!(
            a.base_image,
            dir.join("gapps.qcow2").to_string_lossy().into_owned()
        );
    }

    #[test]
    fn reresolve_leaves_a_manually_entered_path_alone() {
        let (_guard, _dir) = AndlerHomeGuard::new();
        let mut basic_result = android_result("/tmp/hand-picked.qcow2".into(), false);

        reresolve_android_base_image(
            &mut basic_result,
            Some(&advanced_with_gapps(true)),
            &sample_detected(),
        );

        let BasicResult::Android(a) = &basic_result else {
            panic!("expected Android");
        };
        assert_eq!(a.base_image, "/tmp/hand-picked.qcow2");
    }

    #[test]
    fn reresolve_keeps_a_downloaded_image_and_warns_when_nothing_matches() {
        // A downloaded build is wizard-picked (auto), so flipping GApps in the
        // advanced pass re-resolves — and, with no GAPPS build in the cache,
        // keeps the downloaded image instead of silently creating a GAPPS
        // profile on a VANILLA disk.
        let (_guard, dir) = AndlerHomeGuard::new();
        write_manifest(&dir, "vanilla", "13", "VANILLA");
        let downloaded = dir.join("vanilla.qcow2").to_string_lossy().into_owned();
        let mut basic_result = android_result(downloaded.clone(), true);

        reresolve_android_base_image(
            &mut basic_result,
            Some(&advanced_with_gapps(true)),
            &sample_detected(),
        );

        let BasicResult::Android(a) = &basic_result else {
            panic!("expected Android");
        };
        assert_eq!(a.base_image, downloaded);
    }

    #[test]
    fn advanced_default_is_the_recommended_configuration() {
        let advanced = AdvancedConfig::default();

        assert_eq!(advanced.cpu_cores, 4);
        assert_eq!(advanced.memory_gib, 8);
        assert!(advanced.clipboard_enabled);
        assert_eq!(advanced.arm_translator, None);
        assert!(matches!(advanced.network_mode, NetworkMode::Nat));
    }
}
