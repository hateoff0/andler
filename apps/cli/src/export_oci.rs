use crate::helpers::emit_json;
use crate::CliDiskFormat;
use crate::TracedClient;
use andler_rpc::proto::ExportInstanceOciRequest;

fn export_oci_json(source_instance_id: &str, dest_path: &str) -> serde_json::Value {
    serde_json::json!({
        "dest_path": dest_path,
        "source_instance_id": source_instance_id,
    })
}

pub async fn handle_export_oci(
    client: &mut TracedClient,
    source_instance_id: String,
    dest_path: String,
    disk_format: CliDiskFormat,
    disk_path: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .export_instance_oci(ExportInstanceOciRequest {
            source_instance_id: source_instance_id.clone(),
            dest_path: dest_path.clone(),
            disk_format: andler_rpc::proto::DiskFormat::from(disk_format) as i32,
            disk_path,
        })
        .await?
        .into_inner();
    if json {
        emit_json(&export_oci_json(&source_instance_id, &response.dest_path))?;
    } else {
        println!("exported to {}", response.dest_path);
    }
    Ok(())
}
