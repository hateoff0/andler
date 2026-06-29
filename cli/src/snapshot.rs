use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    CreateSnapshotRequest, DeleteSnapshotRequest, InstanceIdRequest, RestoreSnapshotRequest,
};
use tonic::transport::Channel;

use crate::SnapshotAction;

pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    action: SnapshotAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SnapshotAction::Create { tag, description, timeout } => {
            let response = client
                .create_snapshot(CreateSnapshotRequest {
                    instance_id,
                    tag,
                    description: description.unwrap_or_default(),
                    timeout_secs: timeout,
                })
                .await?
                .into_inner();
            println!(
                "snapshot created: tag={}, id={}, created_at={}",
                response.tag, response.snapshot_id, response.created_at
            );
        }
        SnapshotAction::Restore { tag, timeout } => {
            let msg = format!("snapshot {tag} restored");
            client
                .restore_snapshot(RestoreSnapshotRequest {
                    instance_id,
                    tag,
                    timeout_secs: timeout,
                })
                .await?;
            println!("{msg}");
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
