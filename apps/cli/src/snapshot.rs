use crate::helpers::emit_json;
use crate::TracedClient;
use andler_rpc::proto::{
    CreateSnapshotRequest, DeleteSnapshotRequest, InstanceIdRequest, RestoreSnapshotRequest,
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

use crate::SnapshotAction;

fn spinner(message: &str) -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    if let Ok(style) = ProgressStyle::default_spinner().template("{spinner} {msg}") {
        pb.set_style(style);
    }
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

pub async fn handle(
    client: &mut TracedClient,
    instance_id: String,
    action: SnapshotAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SnapshotAction::Create {
            instance_id: _,
            tag,
            description,
            timeout,
        } => {
            let pb = spinner(&format!("Creating snapshot \"{tag}\"..."));
            let result = client
                .create_snapshot(CreateSnapshotRequest {
                    instance_id,
                    tag,
                    description: description.unwrap_or_default(),
                    timeout_secs: timeout,
                })
                .await;
            pb.finish_and_clear();
            let response = result?.into_inner();
            println!(
                "snapshot created: tag={}, id={}, created_at={}",
                response.tag, response.snapshot_id, response.created_at
            );
        }
        SnapshotAction::Restore {
            instance_id: _,
            tag,
            timeout,
            branch,
            idempotency_token,
        } => {
            let pb = spinner(&format!("Restoring snapshot \"{tag}\"..."));
            let result = client
                .restore_snapshot(RestoreSnapshotRequest {
                    instance_id,
                    tag: tag.clone(),
                    timeout_secs: timeout,
                    branch,
                    idempotency_token,
                })
                .await;
            pb.finish_and_clear();
            result?;
            println!("snapshot {tag} restored");
        }
        SnapshotAction::Delete {
            instance_id: _,
            tag,
            timeout,
        } => {
            if std::io::stdin().is_terminal() {
                let confirmed = inquire::Confirm::new(&format!(
                    "This permanently deletes snapshot {tag:?} of instance {instance_id}. Continue?"
                ))
                .with_default(false)
                .prompt()
                .map_err(|e| format!("delete aborted: {e}"))?;
                if !confirmed {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            let msg = format!("snapshot {tag} deleted");
            client
                .delete_snapshot(DeleteSnapshotRequest {
                    instance_id,
                    tag,
                    timeout_secs: timeout,
                })
                .await?;
            println!("{msg}");
        }
        SnapshotAction::List { instance_id: _ } => {
            let response = client
                .list_snapshots(InstanceIdRequest { instance_id })
                .await?
                .into_inner();
            if json {
                #[derive(serde::Serialize)]
                struct SnapshotJson<'a> {
                    tag: &'a str,
                    snapshot_id: &'a str,
                    created_at: &'a str,
                    description: &'a str,
                    branch: &'a str,
                }
                let entries: Vec<SnapshotJson> = response
                    .snapshots
                    .iter()
                    .map(|snap| SnapshotJson {
                        tag: &snap.tag,
                        snapshot_id: &snap.snapshot_id,
                        created_at: &snap.created_at,
                        description: &snap.description,
                        branch: &snap.branch,
                    })
                    .collect();
                emit_json(&entries)?;
                return Ok(());
            }
            if response.snapshots.is_empty() {
                println!("no snapshots");
            } else {
                for snap in &response.snapshots {
                    let branch_suffix = if snap.branch.is_empty() {
                        String::new()
                    } else {
                        format!(", branch={}", snap.branch)
                    };
                    println!(
                        "tag={}, id={}, created_at={}, description={}{}",
                        snap.tag,
                        snap.snapshot_id,
                        crate::helpers::format_timestamp(&snap.created_at),
                        if snap.description.is_empty() {
                            "-"
                        } else {
                            &snap.description
                        },
                        branch_suffix
                    );
                }
            }
        }
    }
    Ok(())
}
