use crate::helpers::{emit_json, state_kind_name};
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

/// Emit the instance's post-transition state, honoring the `--json` contract:
/// on request, emit a JSON document (the single source of truth); otherwise
/// print the human-readable "{verb} {id} ({name})" line. Called after
/// start/stop/pause/resume so a caller gets the authoritative state.
async fn emit_status(
    client: &mut TracedClient,
    id: &str,
    name: Option<&str>,
    verb: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_instance_status(InstanceIdRequest {
            instance_id: id.to_string(),
        })
        .await?
        .into_inner();
    if json {
        emit_json(&serde_json::json!({
            "instance_id": crate::helpers::short_id(id),
            "state": state_kind_name(response.state()),
            "detail": response.detail,
            "error_message": response.error_message,
        }))?;
    } else {
        print_echo(verb, id, name);
    }
    Ok(())
}

pub async fn handle_start(
    client: &mut TracedClient,
    instance_id: String,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .start_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    emit_status(client, &id, name.as_deref(), "started", json).await
}

pub async fn handle_stop(
    client: &mut TracedClient,
    instance_id: String,
    graceful: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .stop_instance(StopInstanceRequest {
            instance_id: instance_id.clone(),
            graceful,
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    emit_status(client, &id, name.as_deref(), "stopped", json).await
}

pub async fn handle_pause(
    client: &mut TracedClient,
    instance_id: String,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .pause_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    emit_status(client, &id, name.as_deref(), "paused", json).await
}

pub async fn handle_resume(
    client: &mut TracedClient,
    instance_id: String,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .resume_instance(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?;
    let (id, name) = resolve_echo(client, &instance_id).await;
    emit_status(client, &id, name.as_deref(), "resumed", json).await
}

pub async fn handle_remove(
    client: &mut TracedClient,
    instance_id: String,
    purge: bool,
    json: bool,
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
    if json {
        emit_json(&serde_json::json!({ "instance_id": crate::helpers::short_id(&id) }))?;
    } else {
        print_echo("removed", &id, name.as_deref());
    }
    Ok(())
}
