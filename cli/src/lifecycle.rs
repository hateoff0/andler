use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{InstanceIdRequest, RemoveInstanceRequest, StopInstanceRequest};
use tonic::transport::Channel;

pub async fn handle_start(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .start_instance(InstanceIdRequest { instance_id })
        .await?;
    println!("started");
    Ok(())
}

pub async fn handle_stop(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    graceful: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .stop_instance(StopInstanceRequest {
            instance_id,
            graceful,
        })
        .await?;
    println!("stopped");
    Ok(())
}

pub async fn handle_pause(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .pause_instance(InstanceIdRequest { instance_id })
        .await?;
    println!("paused");
    Ok(())
}

pub async fn handle_resume(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .resume_instance(InstanceIdRequest { instance_id })
        .await?;
    println!("resumed");
    Ok(())
}

pub async fn handle_remove(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    purge: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .remove_instance(RemoveInstanceRequest {
            instance_id,
            purge,
        })
        .await?;
    println!("removed");
    Ok(())
}
