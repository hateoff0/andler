use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{CloneInstanceRequest, ExportInstanceDiskRequest};
use tonic::transport::Channel;

use crate::CliCloneMode;

pub async fn handle_clone(
    client: &mut AndlerServiceClient<Channel>,
    source_instance_id: String,
    name: String,
    instances_root: String,
    mode: CliCloneMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id,
            new_name: name,
            instances_root,
            mode: andler_rpc::proto::CloneMode::from(mode) as i32,
        })
        .await?
        .into_inner();
    println!(
        "cloned instance_id={}",
        crate::helpers::short_id(&response.instance_id)
    );
    Ok(())
}

pub async fn handle_export(
    client: &mut AndlerServiceClient<Channel>,
    source_instance_id: String,
    dest_path: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .export_instance_disk(ExportInstanceDiskRequest {
            source_instance_id,
            dest_path,
        })
        .await?
        .into_inner();
    println!("exported to {}", response.dest_path);
    Ok(())
}
