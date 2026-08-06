use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidBootMode as ProtoAndroidBootMode, InstallGuestAgentRequest, InstanceIdRequest,
    RemoveGuestAgentRequest, SwitchAndroidBootModeRequest, SwitchArmTranslatorRequest,
};
use tonic::transport::Channel;

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
    },

    Remove {
        package: String,

        instance_id: String,
    },

    BootMode {
        instance_id: String,

        #[arg(value_enum)]
        mode: Option<CliBootMode>,
    },
}

pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    action: GuestAction,
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

            if packages.is_empty() {
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
                };
                client.install_guest_agent(request).await?;
                println!("Package `{package}` installed successfully");
            }
        }
        GuestAction::Remove {
            package,
            instance_id,
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
                };
                client.remove_guest_agent(request).await?;
                println!("Package `{package}` removed successfully");
            }
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
