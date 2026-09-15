pub(crate) mod advanced;
mod apply;
mod base_image;
mod basic;
pub(crate) mod build;
mod summary;
mod ui;

use andler_core::config::HostFirmware;
use andler_core::{AndroidBootMode, ArmTranslator};
use andler_firmware::{FirmwareError, HardwareDefaults};

use crate::create::Creation;
use crate::{CliAndroidVersion, CliArmTranslator, CliCdromBus, TracedClient};

pub use basic::{AndroidBasicResult, BasicResult, LinuxBasicResult};

/// The wizard's answers resolved by the one resolver: what the summary screen
/// showed and what creation sends are the same config.
#[derive(Debug, Clone)]
pub(crate) struct WizardResult {
    pub creation: Creation,
}

/// The CLI-flag layer every creation path reads: whatever the operator put on
/// the command line, independent of which question flow (or none) turns it
/// into a config. `--quick` consults it instead of the questions, the
/// interactive wizard prefills its questions with it, and the flag path builds
/// its draft from it directly.
#[derive(Default, Clone)]
pub struct PartialArgs {
    pub kind: Option<WizardKind>,
    pub name: Option<String>,
    pub iso_path: Option<String>,
    pub base_image_path: Option<String>,
    pub instances_root: Option<String>,
    pub disk_path: Option<String>,
    pub disk_size_gib: Option<u64>,
    pub overlay_size_gib: Option<u64>,
    pub android_version: Option<CliAndroidVersion>,
    pub arm_translator: Option<CliArmTranslator>,
    pub cdrom_bus: Option<CliCdromBus>,
    pub compact_on_shutdown: Option<bool>,
    pub enable_uefi: Option<bool>,
    pub ovmf_vars_template: Option<String>,
    pub microg: bool,
    pub gapps: bool,
    pub linked: bool,
    pub template: Option<String>,
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

/// Resolves the wizard's answers into the one creation — the interactive flow
/// when there is a TTY, the flags-and-defaults flow under `--quick`.
pub(crate) async fn run(
    client: &mut TracedClient,
    partial: PartialArgs,
) -> Result<WizardResult, WizardError> {
    let detected = andler_firmware::detect_all();
    let host = crate::create::host_firmware(&detected);

    if partial.quick {
        return build_quick(&partial, &detected, &host);
    }

    if !is_tty() {
        return Err(WizardError::NotTty);
    }

    if partial.kind == Some(WizardKind::Android) && detected.ovmf.is_err() {
        return Err(WizardError::Firmware(
            detected
                .ovmf
                .err()
                .unwrap_or(FirmwareError::OvmfVarsNotFound),
        ));
    }

    ui::intro("andler · create a virtual machine")?;
    ui::hardware_screen(&detected, partial.kind);

    let mode = ask_wizard_mode()?;
    let kind = basic::ask_kind(partial.kind)?;
    let name = basic::ask_name(partial.name.clone(), kind)?;

    let mut basic_result = match kind {
        WizardKind::Linux => BasicResult::Linux(basic::run_linux(
            name,
            partial.iso_path.clone(),
            partial.instances_root.clone(),
            partial.disk_size_gib,
        )?),
        WizardKind::Android => BasicResult::Android(
            basic::run_android(
                client,
                name,
                partial.base_image_path.clone(),
                partial.instances_root.clone(),
                partial.disk_size_gib,
            )
            .await?,
        ),
    };

    let mut advanced_config = match mode {
        WizardMode::Advanced => Some(match &basic_result {
            BasicResult::Linux(l) => advanced::run_linux(l, &detected, None)?,
            BasicResult::Android(a) => advanced::run_android(a, &detected, None)?,
        }),
        WizardMode::Basic => None,
    };
    build::reresolve_android_base_image(&mut basic_result, advanced_config.as_ref(), &detected)?;

    loop {
        match summary::run(&basic_result, advanced_config.as_ref(), &detected)? {
            summary::SummaryAction::Create => break,
            summary::SummaryAction::Modify => {
                // The modify pass asks which groups to revisit and re-asks
                // only those, keeping everything else as configured.
                advanced_config = Some(match &basic_result {
                    BasicResult::Linux(l) => {
                        advanced::run_linux(l, &detected, advanced_config.as_ref())?
                    }
                    BasicResult::Android(a) => {
                        advanced::run_android(a, &detected, advanced_config.as_ref())?
                    }
                });
                build::reresolve_android_base_image(
                    &mut basic_result,
                    advanced_config.as_ref(),
                    &detected,
                )?;
            }
            summary::SummaryAction::Cancel => return Err(WizardError::Cancelled),
        }
    }

    let draft = match &basic_result {
        BasicResult::Linux(l) => {
            if detected.ovmf.is_err() {
                ui::warn(
                    "OVMF not found: the VM will boot with legacy BIOS. Install edk2-ovmf \
                     for UEFI support.",
                )?;
            }
            build::linux_draft(l, advanced_config.as_ref(), &partial, &detected)?
        }
        BasicResult::Android(a) => {
            build::android_draft(a, advanced_config.as_ref(), &partial, &detected)?
        }
    };

    resolve_draft(draft, &partial, &host)
}

/// Turns a draft into the one creation, with the validation verdict every
/// consumer reports: the wizard refuses a config the daemon would reject,
/// exactly like `--dry-run`, `--verify` and the CLI-flag path.
pub(crate) fn resolve_draft(
    draft: andler_core::config::ConfigDraft,
    flags: &PartialArgs,
    host: &HostFirmware,
) -> Result<WizardResult, WizardError> {
    let instances_root = flags
        .instances_root
        .clone()
        .unwrap_or_else(crate::create::default_instances_root);
    let creation =
        Creation::resolve(&draft, instances_root, host).map_err(WizardError::InvalidConfig)?;
    creation.validation().map_err(WizardError::InvalidConfig)?;
    Ok(WizardResult { creation })
}

fn ask_wizard_mode() -> Result<WizardMode, WizardError> {
    let mode = cliclack::select("How much do you want to configure?")
        .item(
            WizardMode::Basic,
            "Recommended settings",
            // Kept short: cliclack draws the hint on the same line as the
            // label and does not wrap it, so list lines have to fit the
            // terminal themselves.
            "essentials only; the rest is auto-detected",
        )
        .item(
            WizardMode::Advanced,
            "Customize everything",
            "every group: GPU, display, CPU, network, guest",
        )
        .initial_value(WizardMode::Basic)
        .interact()?;

    Ok(mode)
}

/// `--quick`: the flags layer and the defaults, no questions. Every create
/// flag reaches the draft here, so `--quick --disk-size-gib 512` means the
/// same thing as the same flag on the CLI-mode path.
fn build_quick(
    flags: &PartialArgs,
    detected: &HardwareDefaults,
    host: &HostFirmware,
) -> Result<WizardResult, WizardError> {
    let kind = flags.kind.ok_or_else(|| {
        WizardError::Message("`--quick` requires `--kind` to specify VM type".into())
    })?;

    match kind {
        WizardKind::Linux => {
            let name = flags
                .name
                .clone()
                .unwrap_or_else(|| "quick-linux".to_string());
            let instances_root = flags
                .instances_root
                .clone()
                .unwrap_or_else(crate::create::default_instances_root);

            let basic = LinuxBasicResult {
                name,
                iso_path: flags.iso_path.clone().unwrap_or_default(),
                disk_size_gib: flags
                    .disk_size_gib
                    .unwrap_or(andler_core::config::DEFAULT_DISK_GIB),
                instances_root,
                enable_uefi: flags.enable_uefi,
            };
            let draft = build::linux_draft(&basic, None, flags, detected)?;
            resolve_draft(draft, flags, host)
        }
        WizardKind::Android => {
            if detected.ovmf.is_err() {
                return Err(WizardError::Firmware(FirmwareError::OvmfVarsNotFound));
            }

            let name = flags
                .name
                .clone()
                .unwrap_or_else(|| "quick-android".to_string());
            let android_version = flags
                .android_version
                .unwrap_or(CliAndroidVersion::Android13);
            let base_image_auto_resolved = flags.base_image_path.is_none();
            let base_image = match &flags.base_image_path {
                Some(path) => {
                    if !std::path::Path::new(path).exists() {
                        return Err(WizardError::Message(format!(
                            "Base image not found: {path}. Android requires a valid base image; \
                             download one with `andler image download`, or build one with \
                             `docker/images/build.sh`."
                        )));
                    }
                    path.clone()
                }
                None => {
                    let quick_profile = andler_core::AndroidProfile {
                        android_version: android_version.into(),
                        gapps: flags.gapps,
                        microg: flags.microg,
                        arm_translator: ArmTranslator::None,
                        boot_mode: AndroidBootMode::Android,
                        base_image_pin: None,
                    };
                    andler_core::base_image::resolve(&quick_profile)
                        .map_err(|e| WizardError::Message(e.to_string()))?
                        .to_string_lossy()
                        .into_owned()
                }
            };

            let instances_root = flags
                .instances_root
                .clone()
                .unwrap_or_else(crate::create::default_instances_root);

            let basic = AndroidBasicResult {
                name,
                base_image,
                base_image_auto_resolved,
                android_version,
                gapps: flags.gapps,
                disk_size_gib: flags
                    .disk_size_gib
                    .unwrap_or(andler_core::config::DEFAULT_DISK_GIB),
                instances_root,
            };
            let draft = build::android_draft(&basic, None, flags, detected)?;
            resolve_draft(draft, flags, host)
        }
    }
}

/// `andler wizard` (and a bare `andler create`): interact, create, apply the
/// guest-side selections, report. Everything after the summary screen lives
/// here so the flag paths and the wizard share one create code path.
pub async fn handle_wizard(
    client: &mut TracedClient,
    partial: PartialArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let quick = partial.quick;
    let result = match run(client, partial).await {
        Ok(result) => result,
        Err(WizardError::Cancelled) => {
            // The prompt that was cancelled printed its own footer; this only
            // says what the cancellation means.
            ui::outro_cancel("Nothing was created.")?;
            return Ok(());
        }
        Err(err @ WizardError::NotTty) => {
            // A usage error, not a crash: scripts get a distinct exit code
            // and the message names the non-interactive alternatives.
            eprintln!("{err}");
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };

    let id = apply::create(client, &result).await?;
    let kind = match &result.creation.cfg.kind {
        andler_core::InstanceKind::LinuxVm { .. } => WizardKind::Linux,
        andler_core::InstanceKind::AndroidVm { .. } => WizardKind::Android,
    };
    // `--quick` is the scripted path: it must not start a multi-MB translator
    // download (or any other guest work) behind the caller's back. The
    // selections stay recorded in the instance's config, and the report names
    // the command that applies them.
    let applied = if quick {
        None
    } else {
        Some(apply::apply_guest_selections(client, &id).await)
    };
    apply::report(&id, kind, applied.as_ref());
    if !quick {
        // Closes the session `run` opened; `--quick` never opened one.
        ui::outro("The instance is ready.")?;
    }

    Ok(())
}

/// Whether the wizard may prompt at all: `ANDLER_WIZARD_NOT_TTY` forces the
/// scripted path, otherwise stdin has to be a terminal.
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

/// A cliclack prompt fails with a plain `io::Error`; the two kinds the wizard
/// reacts to are the two the library uses for its own states — `Interrupted`
/// is Esc/Ctrl-C, `NotConnected` is "this is not a terminal". Everything else
/// is a real I/O failure and keeps its message.
impl From<std::io::Error> for WizardError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotConnected => WizardError::NotTty,
            std::io::ErrorKind::Interrupted => WizardError::Cancelled,
            _ => WizardError::Message(e.to_string()),
        }
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
    Message(String),

    #[error("{0}")]
    InvalidConfig(String),

    #[error("{0}")]
    Firmware(#[from] FirmwareError),
}

// The guard both test modules take: ANDLER_HOME is process-wide, so a test
// that points it at a scratch cache must not run beside another one.
#[cfg(test)]
static ANDLER_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn lazy_client() -> TracedClient {
        // Nothing in these tests performs an RPC: the client exists so the
        // wizard's signature matches the interactive flow. A lazy channel
        // never dials, so no daemon is required.
        let channel = tonic::transport::Channel::from_static("http://127.0.0.1:1").connect_lazy();
        andler_rpc::proto::andler_service_client::AndlerServiceClient::with_interceptor(
            channel,
            crate::RequestIdInterceptor,
        )
    }

    use std::path::PathBuf;

    #[tokio::test]
    async fn test_wizard_not_tty() {
        std::env::set_var("ANDLER_WIZARD_NOT_TTY", "1");
        let partial = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("interactive-test".into()),
            quick: false,
            ..Default::default()
        };
        let mut client = lazy_client();

        let result = run(&mut client, partial).await;

        std::env::remove_var("ANDLER_WIZARD_NOT_TTY");
        assert!(matches!(result, Err(WizardError::NotTty)));
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
        let detected = no_ovmf();
        let host = crate::create::host_firmware(&detected);
        let result = build_quick(&partial, &detected, &host);
        assert!(result.is_ok());
        let creation = result.unwrap().creation;
        assert_eq!(creation.cfg.name, "quick-linux-test");
        assert!(
            !creation.cfg.firmware.enable_uefi,
            "no OVMF pair means Legacy BIOS, never an empty pflash path"
        );
    }

    #[test]
    fn quick_linux_honours_the_cli_flags() {
        let partial = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("quick-flags".into()),
            quick: true,
            instances_root: Some("/tmp/instances".into()),
            disk_size_gib: Some(512),
            overlay_size_gib: Some(64),
            compact_on_shutdown: Some(true),
            cdrom_bus: Some(crate::CliCdromBus::Ide),
            ..Default::default()
        };
        let detected = detected();
        let host = crate::create::host_firmware(&detected);
        let creation = build_quick(&partial, &detected, &host)
            .expect("quick must resolve")
            .creation;

        assert_eq!(
            creation.cfg.disk.size_bytes,
            512 * andler_core::DiskConfig::GIB,
            "--quick must honour --disk-size-gib instead of the fixed default"
        );
        assert!(
            creation.cfg.disk.compact_on_shutdown,
            "--quick must honour --compact-on-shutdown"
        );
        let andler_core::InstanceKind::LinuxVm { cdrom_bus, .. } = creation.cfg.kind else {
            panic!("expected a Linux VM");
        };
        assert_eq!(
            cdrom_bus,
            andler_core::CdromBus::Ide,
            "--quick must honour --cdrom-bus"
        );
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
        let detected = no_ovmf();
        let host = crate::create::host_firmware(&detected);
        let result = build_quick(&partial, &detected, &host);
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
        let detected = detected();
        let host = crate::create::host_firmware(&detected);
        let result = build_quick(&partial, &detected, &host);
        assert!(matches!(
            result,
            Err(WizardError::Message(msg)) if msg.contains("Base image not found")
        ));
    }

    #[test]
    fn test_quick_android_forward_linked_overlay_flag() {
        let dir =
            std::env::temp_dir().join(format!("andler-wizard-quick-linked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.qcow2");
        std::fs::write(&base, b"stand-in base image for the linked-overlay test").unwrap();
        let partial = PartialArgs {
            kind: Some(WizardKind::Android),
            name: Some("quick-linked".into()),
            base_image_path: Some(base.to_string_lossy().into_owned()),
            instances_root: Some(dir.join("instances").to_string_lossy().into_owned()),
            quick: true,
            linked: true,
            ..Default::default()
        };
        let detected = detected();
        let host = crate::create::host_firmware(&detected);
        let result = build_quick(&partial, &detected, &host);
        match result {
            Ok(result) => {
                assert!(
                    result.creation.cfg.disk.base_image.is_some(),
                    "quick create must forward --linked-overlay into the create request"
                );
            }
            Err(e) => panic!("build_quick failed: {e}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quick_android_without_a_base_image_reports_the_resolve_error() {
        // cwd-independent: point ANDLER_HOME at an empty cache so no image
        // can match, exactly like a fresh install.
        let guard = EmptyCacheGuard::new();
        let partial = PartialArgs {
            kind: Some(WizardKind::Android),
            name: Some("quick-no-image".into()),
            quick: true,
            instances_root: Some(guard.base.join("instances").to_string_lossy().into_owned()),
            ..Default::default()
        };

        let detected = detected();
        let host = crate::create::host_firmware(&detected);
        let result = build_quick(&partial, &detected, &host);

        match result {
            Err(WizardError::Message(message)) => assert!(
                message.contains("no base image found"),
                "the resolve error must say no image matched: {message}"
            ),
            other => panic!("expected a base-image resolve failure, got {other:?}"),
        }
    }

    fn detected() -> HardwareDefaults {
        build::sample_detected()
    }

    fn no_ovmf() -> HardwareDefaults {
        HardwareDefaults {
            ovmf: Err(FirmwareError::OvmfVarsNotFound),
            ..detected()
        }
    }

    struct EmptyCacheGuard {
        base: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EmptyCacheGuard {
        fn new() -> Self {
            let lock = super::ANDLER_HOME_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let base = std::env::temp_dir().join(format!(
                "andler-wizard-empty-cache-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(base.join("cache/base-images")).unwrap();
            std::env::set_var(andler_core::paths::ANDLER_HOME_ENV, &base);
            Self { base, _lock: lock }
        }
    }

    impl Drop for EmptyCacheGuard {
        fn drop(&mut self) {
            std::env::remove_var(andler_core::paths::ANDLER_HOME_ENV);
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
}
