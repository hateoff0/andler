use crate::helpers::emit_json;
use crate::TracedClient;
use andler_rpc::proto::{
    AndroidBootMode as ProtoAndroidBootMode, ApplyGuestProfileResponse,
    ArmTranslator as ProtoArmTranslator, GuestPackageEntry, GuestProfileEntry, GuestProfileStatus,
    GuestProvisionRequest, InstallGuestAgentRequest, InstanceIdRequest, RemoveGuestAgentRequest,
    SwitchAndroidBootModeRequest, SwitchArmTranslatorRequest,
};

use std::io::IsTerminal;

use crate::lifecycle;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum CliBootMode {
    Android,
    Linux,
}

impl From<CliBootMode> for ProtoAndroidBootMode {
    fn from(value: CliBootMode) -> Self {
        match value {
            CliBootMode::Android => ProtoAndroidBootMode::Android,
            CliBootMode::Linux => ProtoAndroidBootMode::Linux,
        }
    }
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum GuestAction {
    List {
        instance_id: Option<String>,
    },

    Install {
        package: String,

        instance_id: String,

        #[arg(long)]
        translator_dir: Option<std::path::PathBuf>,

        /// Force the offline path (the zero-root libguestfs appliance)
        /// instead of the smart path: install via the guest agent,
        /// auto-starting a stopped VM for maintenance when needed.
        #[arg(long)]
        offline: bool,

        /// Client-supplied idempotency key: a network retry with the same
        /// token joins the in-flight install instead of starting a second one.
        #[arg(long)]
        idempotency_token: Option<String>,
    },

    Remove {
        package: String,

        instance_id: String,

        /// Force the offline path (the zero-root libguestfs appliance)
        /// instead of the smart path: remove via the guest agent,
        /// auto-starting a stopped VM for maintenance when needed.
        #[arg(long)]
        offline: bool,

        /// Client-supplied idempotency key: a network retry with the same
        /// token joins the in-flight remove instead of starting a second one.
        #[arg(long)]
        idempotency_token: Option<String>,
    },

    /// Apply a provision manifest (docker/images/guest-components/*/manifest.toml).
    /// Runs online via the guest agent when the VM is running, offline via
    /// the guestfs appliance when it is stopped.
    Provision {
        manifest: std::path::PathBuf,

        instance_id: String,
    },

    BootMode {
        instance_id: String,

        #[arg(value_enum)]
        mode: Option<CliBootMode>,
    },

    /// Apply every guest-side setting the instance's own config asks for
    /// (ARM translator, clipboard agent) — what the wizard does right after
    /// creating a VM
    Apply {
        instance_id: String,
    },
}

fn package_json(pkg: &GuestPackageEntry) -> serde_json::Value {
    serde_json::json!({
        "name": pkg.name,
        "description": pkg.description,
        "status": pkg.status,
    })
}

fn packages_json(packages: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({ "packages": packages })
}

// The translator names the guest commands switch on: `none` disables ARM
// translation (the removal path) instead of naming a guest package.
fn translator_named(name: &str) -> Option<ProtoArmTranslator> {
    match name {
        "none" => Some(ProtoArmTranslator::None),
        "libndk" => Some(ProtoArmTranslator::Libndk),
        "libhoudini" => Some(ProtoArmTranslator::Libhoudini),
        _ => None,
    }
}

async fn switch_arm_translator(
    client: &mut TracedClient,
    instance_ref: String,
    translator: ProtoArmTranslator,
    translator_dir: Option<&std::path::Path>,
    progress: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = SwitchArmTranslatorRequest {
        instance_ref: instance_ref.clone(),
        translator: translator.into(),
        translator_dir: translator_dir
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let mut call_client = client.clone();
    crate::helpers::call_with_operation_progress(
        client,
        &instance_ref,
        progress,
        |line| println!("  {line}"),
        async move { call_client.switch_arm_translator(request).await },
    )
    .await?;
    Ok(())
}

pub async fn handle(
    client: &mut TracedClient,
    action: GuestAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        GuestAction::List { instance_id: None } => {
            println!("Usage: andler guest list <instance-id>");
            println!("       andler guest install <package> <instance-id>");
            println!("       andler guest remove <package> <instance-id>");
            println!("       andler guest boot-mode <instance-id> [android|linux]");
            println!("       andler guest apply <instance-id>");
        }
        GuestAction::List {
            instance_id: Some(id),
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &id).await;
            let request = InstanceIdRequest {
                instance_id: resolved_id.clone(),
            };
            let mut call_client = client.clone();
            let response = crate::helpers::call_with_operation_progress(
                client,
                &resolved_id,
                "reading the guest disk (a stopped instance starts a libguestfs appliance)",
                |line| println!("  {line}"),
                async move { call_client.list_guest_packages(request).await },
            )
            .await?;
            let packages = response.into_inner().packages;
            let rows: Vec<serde_json::Value> = packages.iter().map(package_json).collect();
            if json {
                emit_json(&packages_json(&rows))?;
            } else if packages.is_empty() {
                println!("No known packages.");
            } else {
                let is_tty = std::io::stdout().is_terminal();
                println!("{:<25} {:<45} Status", "Package", "Description");
                println!("{}", "-".repeat(80));
                for pkg in &packages {
                    let status_str = if is_tty {
                        match pkg.status.as_str() {
                            "installed" => "\x1b[32minstalled\x1b[0m",
                            "not_installed" => "\x1b[31mnot installed\x1b[0m",
                            _ => "\x1b[33munknown\x1b[0m",
                        }
                    } else {
                        match pkg.status.as_str() {
                            "installed" => "installed",
                            "not_installed" => "not installed",
                            _ => "unknown",
                        }
                    };
                    println!("{:<25} {:<45} {}", pkg.name, pkg.description, status_str);
                }
            }
        }
        GuestAction::Install {
            package,
            instance_id,
            translator_dir,
            offline,
            idempotency_token,
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;

            if let Some(translator) = translator_named(&package) {
                let disables = translator == ProtoArmTranslator::None;
                if disables {
                    println!("Setting the ARM translator to `none` (no payload to download) …");
                } else {
                    match &translator_dir {
                        Some(dir) => println!(
                            "Installing ARM translator `{package}` from {} …",
                            dir.display()
                        ),
                        None => println!(
                            "Installing ARM translator `{package}` (downloads ~18 MiB on first run; this can take a while) …"
                        ),
                    }
                }
                switch_arm_translator(
                    client,
                    resolved_id,
                    translator,
                    translator_dir.as_deref(),
                    if disables {
                        "disabling the ARM translator"
                    } else {
                        "installing the ARM translator"
                    },
                )
                .await?;
                if disables {
                    println!("Translator `none` set — ARM translation is off");
                } else {
                    match &translator_dir {
                        Some(dir) => {
                            println!("Translator `{package}` installed from {}", dir.display())
                        }
                        None => println!("Translator `{package}` installed (auto-download)"),
                    }
                }
            } else {
                let request = InstallGuestAgentRequest {
                    instance_id: resolved_id.clone(),
                    package: package.clone(),
                    offline,
                    idempotency_token,
                };
                let mut call_client = client.clone();
                crate::helpers::call_with_operation_progress(
                    client,
                    &resolved_id,
                    "installing the package inside the guest",
                    |line| println!("  {line}"),
                    async move { call_client.install_guest_agent(request).await },
                )
                .await?;
                println!("Package `{package}` installed successfully");
            }
        }
        GuestAction::Remove {
            package,
            instance_id,
            offline,
            idempotency_token,
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;
            if translator_named(&package).is_some() {
                switch_arm_translator(
                    client,
                    resolved_id,
                    ProtoArmTranslator::None,
                    None,
                    "removing the ARM translator",
                )
                .await?;
                println!("Translator `{package}` removed");
            } else {
                let request = RemoveGuestAgentRequest {
                    instance_id: resolved_id.clone(),
                    package: package.clone(),
                    offline,
                    idempotency_token,
                };
                let mut call_client = client.clone();
                crate::helpers::call_with_operation_progress(
                    client,
                    &resolved_id,
                    "removing the package from the guest",
                    |line| println!("  {line}"),
                    async move { call_client.remove_guest_agent(request).await },
                )
                .await?;
                println!("Package `{package}` removed successfully");
            }
        }
        GuestAction::Provision {
            manifest,
            instance_id,
        } => {
            let manifest_text = std::fs::read_to_string(&manifest)
                .map_err(|e| format!("cannot read manifest {}: {e}", manifest.display()))?;
            let parsed = andler_core::provision::ProvisionManifest::parse(&manifest_text)?;
            let mut ops = parsed.to_mutator_ops()?;
            let manifest_dir = manifest
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            for op in ops.iter_mut() {
                if let andler_core::MutatorOp::UploadFile { host_path, .. } = op {
                    if host_path.is_relative() {
                        *host_path = manifest_dir.join(&*host_path);
                    }
                }
            }
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;
            let request = GuestProvisionRequest {
                instance_id: resolved_id.clone(),
                ops: andler_rpc::provision_convert::provision_ops_to_proto(&ops),
            };
            let mut call_client = client.clone();
            crate::helpers::call_with_operation_progress(
                client,
                &resolved_id,
                "applying the provision manifest",
                |line| println!("  {line}"),
                async move { call_client.guest_provision(request).await },
            )
            .await?;
            println!(
                "Provisioned `{}` ({} ops) successfully",
                parsed.name,
                ops.len()
            );
        }
        GuestAction::BootMode {
            instance_id,
            mode: None,
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;
            let response = client
                .get_android_boot_mode(InstanceIdRequest {
                    instance_id: resolved_id,
                })
                .await?;
            match response.into_inner().mode() {
                ProtoAndroidBootMode::Android => println!("android"),
                ProtoAndroidBootMode::Linux => println!("linux"),
                ProtoAndroidBootMode::Unspecified => println!("unknown"),
            }
        }
        GuestAction::BootMode {
            instance_id,
            mode: Some(mode),
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;
            let request = SwitchAndroidBootModeRequest {
                instance_ref: resolved_id.clone(),
                mode: ProtoAndroidBootMode::from(mode).into(),
            };
            let mut call_client = client.clone();
            crate::helpers::call_with_operation_progress(
                client,
                &resolved_id,
                "switching the boot mode on the guest disk",
                |line| println!("  {line}"),
                async move { call_client.switch_android_boot_mode(request).await },
            )
            .await?;
            let mode_str = match mode {
                CliBootMode::Android => "android",
                CliBootMode::Linux => "linux",
            };
            println!("Boot mode switched to `{mode_str}`. Restart the instance to apply it.");
        }
        GuestAction::Apply { instance_id } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;
            let request = InstanceIdRequest {
                instance_id: resolved_id.clone(),
            };
            let mut call_client = client.clone();
            let response = crate::helpers::call_with_operation_progress(
                client,
                &resolved_id,
                "applying the guest selections",
                |line| println!("  {line}"),
                async move { call_client.apply_guest_profile(request).await },
            )
            .await?
            .into_inner();
            print_apply_results(&response, json)?;
        }
    }

    Ok(())
}

fn apply_entry_json(entry: &GuestProfileEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "status": profile_status_name(entry.status()).to_string(),
        "message": entry.message,
    })
}

fn profile_status_name(status: GuestProfileStatus) -> &'static str {
    match status {
        GuestProfileStatus::Applied => "applied",
        GuestProfileStatus::AlreadyPresent => "already_present",
        GuestProfileStatus::Skipped => "skipped",
        GuestProfileStatus::Failed => "failed",
        GuestProfileStatus::Unspecified => "unspecified",
    }
}

fn print_apply_results(
    response: &ApplyGuestProfileResponse,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        let entries: Vec<serde_json::Value> =
            response.entries.iter().map(apply_entry_json).collect();
        emit_json(&serde_json::json!({ "selections": entries }))?;
        return Ok(());
    }

    if response.entries.is_empty() {
        println!("Nothing to apply: this instance's config selects no guest-side packages.");
        return Ok(());
    }

    let mut failed = false;
    for entry in &response.entries {
        let (mark, label) = match entry.status() {
            GuestProfileStatus::Applied => ("✓", "applied"),
            GuestProfileStatus::AlreadyPresent => ("•", "already present"),
            GuestProfileStatus::Skipped => ("-", "skipped"),
            GuestProfileStatus::Failed => ("✗", "failed"),
            GuestProfileStatus::Unspecified => ("?", "unknown"),
        };
        if entry.status() == GuestProfileStatus::Failed {
            failed = true;
        }
        println!("{mark} {}: {label} — {}", entry.name, entry.message);
    }

    if failed {
        return Err("some selections could not be applied; the commands above retry them".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{package_json, packages_json, translator_named};
    use andler_rpc::proto::{ArmTranslator, GuestPackageEntry};

    #[test]
    fn translator_names_switch_the_translator_instead_of_being_installed() {
        // `none` is a translator to switch to (the removal path), not a
        // package: routing it through the package path made `guest install
        // none` boot a maintenance VM to install a Debian package called
        // `none` and fail with "target not found: none".
        assert_eq!(translator_named("none"), Some(ArmTranslator::None));
        assert_eq!(translator_named("libndk"), Some(ArmTranslator::Libndk));
        assert_eq!(
            translator_named("libhoudini"),
            Some(ArmTranslator::Libhoudini)
        );
    }

    #[test]
    fn package_names_are_not_translators() {
        assert_eq!(translator_named("spice-vdagent"), None);
        assert_eq!(translator_named("qemu-guest-agent"), None);
        assert_eq!(translator_named("libndk2"), None);
    }

    #[test]
    fn package_json_serializes_all_fields() {
        let pkg = GuestPackageEntry {
            name: "guest-tools".to_string(),
            description: "guest tools".to_string(),
            status: "installed".to_string(),
        };
        let json = package_json(&pkg);
        assert_eq!(json["name"], "guest-tools");
        assert_eq!(json["description"], "guest tools");
        assert_eq!(json["status"], "installed");
    }

    #[test]
    fn packages_json_wraps_rows() {
        let rows: Vec<serde_json::Value> = vec![package_json(&GuestPackageEntry {
            name: "a".to_string(),
            description: "b".to_string(),
            status: "installed".to_string(),
        })];
        let json = packages_json(&rows);
        assert!(json["packages"].is_array());
        assert_eq!(json["packages"][0]["name"], "a");
    }
}
