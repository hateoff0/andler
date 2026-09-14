mod advanced;
mod apply;
mod base_image;
mod basic;
mod build;
mod summary;
mod ui;

use andler_core::{AndroidBootMode, ArmTranslator};
use andler_firmware::{FirmwareError, HardwareDefaults};
use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};
use inquire::{InquireError, Select};

use crate::{CliAndroidVersion, TracedClient};

pub use basic::{AndroidBasicResult, BasicResult, LinuxBasicResult};

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // carries full proto requests; boxing would complicate callers
pub enum WizardResult {
    Linux(CreateInstanceRequest),
    Android(CreateAndroidInstanceRequest),
}

#[derive(Default)]
pub struct PartialArgs {
    pub kind: Option<WizardKind>,
    pub name: Option<String>,
    pub iso_path: Option<String>,
    pub base_image_path: Option<String>,
    pub instances_root: Option<String>,
    pub gapps: bool,
    pub quick: bool,
    pub linked: bool,
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

/// Resolves the wizard's answers into the request to create — the interactive
/// flow when there is a TTY, the defaults-only flow under `--quick`.
pub async fn run(
    client: &mut TracedClient,
    partial: PartialArgs,
) -> Result<WizardResult, WizardError> {
    let detected = andler_firmware::detect_all();

    if partial.quick {
        return build_quick(partial, &detected);
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

    ui::hardware_screen(&detected, partial.kind);

    let mode = ask_wizard_mode()?;
    let kind = basic::ask_kind(partial.kind)?;
    let name = basic::ask_name(partial.name, kind)?;

    let mut basic_result = match kind {
        WizardKind::Linux => BasicResult::Linux(basic::run_linux(
            name,
            partial.iso_path,
            partial.instances_root,
        )?),
        WizardKind::Android => BasicResult::Android(
            basic::run_android(
                client,
                name,
                partial.base_image_path,
                partial.instances_root,
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
    build::reresolve_android_base_image(&mut basic_result, advanced_config.as_ref(), &detected);

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
                );
            }
            summary::SummaryAction::Cancel => return Err(WizardError::Cancelled),
        }
    }

    match &basic_result {
        BasicResult::Linux(l) => {
            if detected.ovmf.is_err() {
                ui::result(
                    ui::Status::Warn,
                    "OVMF not found. Legacy BIOS will be used. \
                     Install edk2-ovmf for UEFI support.",
                );
            }
            let req = build::build_linux_request(l, advanced_config.as_ref(), &detected)?;
            Ok(WizardResult::Linux(req))
        }
        BasicResult::Android(a) => {
            let req = build::build_android_request(a, advanced_config.as_ref(), &detected)?;
            Ok(WizardResult::Android(req))
        }
    }
}

fn ask_wizard_mode() -> Result<WizardMode, WizardError> {
    let recommended = "Recommended settings (basic)";
    let custom = "Customize everything (advanced)";
    let choice = Select::new("Configuration mode:", vec![recommended, custom])
        .with_help_message(
            "Basic — only the essential questions; everything else comes from hardware \
             detection.\nAdvanced — full control over GPU, display, audio, CPU, memory, \
             network and the guest packages.",
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(if choice == recommended {
        WizardMode::Basic
    } else {
        WizardMode::Advanced
    })
}

fn build_quick(
    partial: PartialArgs,
    detected: &HardwareDefaults,
) -> Result<WizardResult, WizardError> {
    let kind = partial.kind.ok_or_else(|| {
        WizardError::Inquire("`--quick` requires `--kind` to specify VM type".into())
    })?;

    match kind {
        WizardKind::Linux => {
            let name = partial.name.unwrap_or_else(|| "quick-linux".to_string());
            let iso = partial.iso_path.unwrap_or_default();
            let instances_root = partial
                .instances_root
                .unwrap_or_else(default_instances_root);

            let enable_uefi = if detected.ovmf.is_err() {
                ui::result(
                    ui::Status::Warn,
                    "OVMF not found. Legacy BIOS will be used. \
                     Install edk2-ovmf for UEFI support.",
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
            let req = build::build_linux_request(&basic, None, detected)?;
            Ok(WizardResult::Linux(req))
        }
        WizardKind::Android => {
            if detected.ovmf.is_err() {
                return Err(WizardError::Firmware(FirmwareError::OvmfVarsNotFound));
            }

            let name = partial.name.unwrap_or_else(|| "quick-android".to_string());
            let base_image_auto_resolved = partial.base_image_path.is_none();
            let base_image = match partial.base_image_path {
                Some(path) => {
                    if !std::path::Path::new(&path).exists() {
                        return Err(WizardError::Inquire(format!(
                            "Base image not found: {path}. Android requires a valid base image; \
                             download one with `andler image download`, or build one with \
                             `docker/images/build.sh`."
                        )));
                    }
                    path
                }
                None => {
                    let quick_profile = andler_core::AndroidProfile {
                        android_version: andler_core::AndroidVersion::Android13,
                        gapps: partial.gapps,
                        microg: false,
                        arm_translator: ArmTranslator::None,
                        boot_mode: AndroidBootMode::Android,
                        base_image_pin: None,
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
                gapps: partial.gapps,
                disk_size_gib: 256,
                instances_root,
                linked: partial.linked,
            };
            let req = build::build_android_request(&basic, None, detected)?;
            Ok(WizardResult::Android(req))
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
            println!("Cancelled.");
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
    let kind = match &result {
        WizardResult::Linux(..) => WizardKind::Linux,
        WizardResult::Android(_) => WizardKind::Android,
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

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
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
        let result = build_quick(partial, &no_ovmf());
        assert!(result.is_ok());
        match result.unwrap() {
            WizardResult::Linux(req) => assert_eq!(req.name, "quick-linux-test"),
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
        let result = build_quick(partial, &no_ovmf());
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
        let result = build_quick(partial, &detected());
        assert!(matches!(
            result,
            Err(WizardError::Inquire(msg)) if msg.contains("Base image not found")
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
        let result = build_quick(partial, &detected());
        match result {
            Ok(WizardResult::Android(req)) => {
                assert!(
                    req.linked_overlay,
                    "quick create must forward --linked-overlay into the create request"
                );
            }
            Ok(WizardResult::Linux(..)) => panic!("expected Android result"),
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

        let result = build_quick(partial, &detected());

        match result {
            Err(WizardError::Inquire(message)) => assert!(
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
