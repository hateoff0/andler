//! Обработка команд `andler guest install/remove/list <package> <instance-id>`.

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{InstallGuestAgentRequest, InstanceIdRequest, RemoveGuestAgentRequest};
use tonic::transport::Channel;

use crate::lifecycle;

#[derive(Debug, Clone, clap::Subcommand)]
pub enum GuestAction {
    /// Install a package in the guest OS.
    Install,
    /// Remove a package from the guest OS.
    Remove,
    /// List known packages and their status in the guest OS.
    List,
}

pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    action: GuestAction,
    package: Option<String>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let (resolved_id, _name) = lifecycle::resolve_echo(client, &instance_id).await;

    match action {
        GuestAction::Install => {
            let pkg = package.ok_or("--package is required for install")?;
            let request = InstallGuestAgentRequest {
                instance_id: resolved_id,
                package: pkg.clone(),
            };
            client.install_guest_agent(request).await?;
            println!("Package `{pkg}` installed successfully");
        }
        GuestAction::Remove => {
            let pkg = package.ok_or("--package is required for remove")?;
            let request = RemoveGuestAgentRequest {
                instance_id: resolved_id,
                package: pkg.clone(),
            };
            client.remove_guest_agent(request).await?;
            println!("Package `{pkg}` removed successfully");
        }
        GuestAction::List => {
            let request = InstanceIdRequest {
                instance_id: resolved_id,
            };
            let response = client.list_guest_packages(request).await?;
            let packages = response.into_inner().packages;

            if packages.is_empty() {
                println!("No known packages.");
            } else {
                println!("{:<25} {:<45} {}", "Package", "Description", "Status");
                println!("{}", "-".repeat(80));
                for pkg in &packages {
                    let status_str = match pkg.status.as_str() {
                        "installed" => "\x1b[32minstalled\x1b[0m",
                        "not_installed" => "\x1b[31mnot installed\x1b[0m",
                        _ => "\x1b[33munknown\x1b[0m",
                    };
                    println!("{:<25} {:<45} {}", pkg.name, pkg.description, status_str);
                }
            }
        }
    }

    Ok(())
}
