use crate::helpers::emit_json;
use crate::TracedClient;
use andler_rpc::proto::{
    AndroidBootMode as ProtoAndroidBootMode, GuestPackageEntry, GuestProvisionRequest,
    InstallGuestAgentRequest, InstanceIdRequest, RemoveGuestAgentRequest,
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

        /// Force the offline path (guestmount FUSE + userns chroot — zero
        /// root) instead of the smart path: install via the guest agent,
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

        /// Force the offline path (guestmount FUSE + userns chroot — zero
        /// root) instead of the smart path: remove via the guest agent,
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
        }
        GuestAction::List {
            instance_id: Some(id),
        } => {
            let (resolved_id, _name) = lifecycle::resolve_echo(client, &id).await;
            let request = InstanceIdRequest {
                instance_id: resolved_id,
            };
            let response = client.list_guest_packages(request).await?;
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

            let is_arm_translator = matches!(package.as_str(), "libndk" | "libhoudini");
            if is_arm_translator {
                let translator = match package.as_str() {
                    "libndk" => andler_rpc::proto::ArmTranslator::Libndk,
                    "libhoudini" => andler_rpc::proto::ArmTranslator::Libhoudini,
                    _ => unreachable!(),
                };
                let dir_str = translator_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match &translator_dir {
                    Some(dir) => {
                        println!("Installing ARM translator `{package}` from {} …", dir.display())
                    }
                    None => println!(
                        "Installing ARM translator `{package}` (downloads ~18 MiB on first run; this can take a while) …"
                    ),
                }
                let request = SwitchArmTranslatorRequest {
                    instance_ref: resolved_id,
                    translator: translator.into(),
                    translator_dir: dir_str,
                };
                client.switch_arm_translator(request).await?;
                match &translator_dir {
                    Some(dir) => {
                        println!("Translator `{package}` installed from {}", dir.display())
                    }
                    None => println!("Translator `{package}` installed (auto-download)"),
                }
            } else {
                let request = InstallGuestAgentRequest {
                    instance_id: resolved_id,
                    package: package.clone(),
                    offline,
                    idempotency_token,
                };
                client.install_guest_agent(request).await?;
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
            let is_arm_translator = matches!(package.as_str(), "libndk" | "libhoudini");
            if is_arm_translator {
                let request = SwitchArmTranslatorRequest {
                    instance_ref: resolved_id,
                    translator: andler_rpc::proto::ArmTranslator::None.into(),
                    translator_dir: String::new(),
                };
                client.switch_arm_translator(request).await?;
                println!("Translator `{package}` removed");
            } else {
                let request = RemoveGuestAgentRequest {
                    instance_id: resolved_id,
                    package: package.clone(),
                    offline,
                    idempotency_token,
                };
                client.remove_guest_agent(request).await?;
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
                instance_id: resolved_id,
                ops: andler_rpc::provision_convert::provision_ops_to_proto(&ops),
            };
            client.guest_provision(request).await?;
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
            client
                .switch_android_boot_mode(SwitchAndroidBootModeRequest {
                    instance_ref: resolved_id,
                    mode: ProtoAndroidBootMode::from(mode).into(),
                })
                .await?;
            let mode_str = match mode {
                CliBootMode::Android => "android",
                CliBootMode::Linux => "linux",
            };
            println!("Boot mode switched to `{mode_str}`. Restart the instance to apply it.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{package_json, packages_json};
    use andler_rpc::proto::GuestPackageEntry;

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
