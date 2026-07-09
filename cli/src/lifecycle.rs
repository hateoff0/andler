use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{InstanceIdRequest, RemoveInstanceRequest, StopInstanceRequest};
use tonic::transport::Channel;

/// Fetches `(full_id, name)` for the instance the user referred to by
/// `instance_id` (which may be a full UUID or a Docker-style prefix —
/// see `Daemon::resolve_instance_id` — resolution happens entirely
/// server-side, there is no separate "resolve" RPC to call first).
///
/// Used to echo which exact instance a lifecycle command affected (see
/// PLAN.md, item 8, "Instance ID echo in success messages") — with
/// prefix-based IDs, printing back just the word "started" leaves the
/// person unsure *which* instance among possibly several matching
/// prefixes actually got started.
///
/// Any failure here (e.g. the daemon restarted between the main call and
/// this lookup, a vanishingly unlikely race) falls back to printing the
/// user's own original `instance_id` string unresolved rather than
/// failing the whole command — the actual lifecycle action already
/// succeeded by the time this runs, so a cosmetic echo failing is not a
/// reason to report the command itself as failed.
pub async fn resolve_echo(
    client: &mut AndlerServiceClient<Channel>,
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
    match name {
        Some(name) => println!("{verb} {id} ({name})"),
        None => println!("{verb} {id}"),
    }
}

pub async fn handle_start(
    client: &mut AndlerServiceClient<Channel>,
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
    client: &mut AndlerServiceClient<Channel>,
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
    client: &mut AndlerServiceClient<Channel>,
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
    client: &mut AndlerServiceClient<Channel>,
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
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    purge: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unlike start/stop/pause/resume, the echo lookup must happen
    // *before* the action here: once `remove_instance` succeeds the
    // instance no longer exists, and `get_instance_config` afterward
    // would just fail (NotFound) — there would be nothing left to
    // resolve the prefix against.
    let (id, name) = resolve_echo(client, &instance_id).await;
    client
        .remove_instance(RemoveInstanceRequest {
            instance_id,
            purge,
        })
        .await?;
    print_echo("removed", &id, name.as_deref());
    Ok(())
}
