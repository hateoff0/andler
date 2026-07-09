use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    CreateSnapshotRequest, DeleteSnapshotRequest, InstanceIdRequest, RestoreSnapshotRequest,
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;
use tonic::transport::Channel;

use crate::SnapshotAction;

/// A spinner for a single blocking gRPC call with no server-side
/// progress data to report (snapshot create/restore go through QMP
/// synchronously — see PLAN.md, item 13, "Snapshot progress
/// indicators" — there's genuinely nothing more granular than "still
/// running" to show). Explicitly hidden when stderr isn't a terminal
/// (piped/redirected output, CI logs) rather than relying on
/// `indicatif`'s own default target — matches how `colorize_status`
/// checks `IsTerminal` itself instead of assuming a library default.
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
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    action: SnapshotAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SnapshotAction::Create { tag, description, timeout } => {
            let pb = spinner(&format!("Creating snapshot \"{tag}\"..."));
            let result = client
                .create_snapshot(CreateSnapshotRequest {
                    instance_id,
                    tag,
                    description: description.unwrap_or_default(),
                    timeout_secs: timeout,
                })
                .await;
            // Finish (clearing the spinner line) before printing the
            // real result/error — an error propagated via `?` after
            // this still leaves a dangling spinner line otherwise,
            // since nothing else would ever call finish()/clear() on it.
            pb.finish_and_clear();
            let response = result?.into_inner();
            println!(
                "snapshot created: tag={}, id={}, created_at={}",
                response.tag, response.snapshot_id, response.created_at
            );
        }
        SnapshotAction::Restore { tag, timeout } => {
            let pb = spinner(&format!("Restoring snapshot \"{tag}\"..."));
            let result = client
                .restore_snapshot(RestoreSnapshotRequest {
                    instance_id,
                    tag: tag.clone(),
                    timeout_secs: timeout,
                })
                .await;
            pb.finish_and_clear();
            result?;
            println!("snapshot {tag} restored");
        }
        SnapshotAction::Delete { tag, timeout } => {
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
        SnapshotAction::List => {
            let response = client
                .list_snapshots(InstanceIdRequest { instance_id })
                .await?
                .into_inner();
            if response.snapshots.is_empty() {
                println!("no snapshots");
            } else {
                for snap in &response.snapshots {
                    println!(
                        "tag={}, id={}, created_at={}, description={}",
                        snap.tag,
                        snap.snapshot_id,
                        snap.created_at,
                        if snap.description.is_empty() {
                            "-"
                        } else {
                            &snap.description
                        }
                    );
                }
            }
        }
    }
    Ok(())
}
