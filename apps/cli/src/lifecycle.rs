use crate::TracedClient;
use andler_rpc::proto::{InstanceIdRequest, RemoveInstanceRequest, StopInstanceRequest};
use std::io::IsTerminal;

pub async fn resolve_echo(
    client: &mut TracedClient,
    instance_id: &str,
) -> (String, Option<String>) {
    match client
        .get_instance_config(InstanceIdRequest {
            instance_id: instance_id.to_string(),
        })
        .await
    {
        Ok(response) => {
            let config = response.into_inner();
            (config.instance_id, Some(config.name))
        }
        Err(_) => (instance_id.to_string(), None),
    }
}

fn print_echo(verb: &str, id: &str, name: Option<&str>) {
    let short = crate::helpers::short_id(id);
    match name {
        Some(name) => println!("{verb} {short} ({name})"),
        None => println!("{verb} {short}"),
    }
}

pub async fn handle_start(
    client: &mut TracedClient,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .start_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    print_echo("started", &id, name.as_deref());
    Ok(())
}

pub async fn handle_stop(
    client: &mut TracedClient,
    instance_id: String,
    graceful: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .stop_instance(StopInstanceRequest {
            instance_id: instance_id.clone(),
            graceful,
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    print_echo("stopped", &id, name.as_deref());
    Ok(())
}

pub async fn handle_pause(
    client: &mut TracedClient,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .pause_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    print_echo("paused", &id, name.as_deref());
    Ok(())
}

pub async fn handle_resume(
    client: &mut TracedClient,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .resume_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    print_echo("resumed", &id, name.as_deref());
    Ok(())
}

pub async fn handle_remove(
    client: &mut TracedClient,
    instance_id: String,
    purge: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (id, name) = resolve_echo(client, &instance_id).await;
    if purge && std::io::stdin().is_terminal() {
        let label = match name.as_deref() {
            Some(name) => format!("{} ({name})", crate::helpers::short_id(&id)),
            None => crate::helpers::short_id(&id).to_string(),
        };
        let confirmed = inquire::Confirm::new(&format!(
            "This permanently deletes instance {label} and its disk image. Continue?"
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| format!("remove aborted: {e}"))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }
    client
        .remove_instance(RemoveInstanceRequest { instance_id, purge })
        .await?;
    print_echo("removed", &id, name.as_deref());
    Ok(())
}
