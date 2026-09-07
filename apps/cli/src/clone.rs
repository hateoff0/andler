use crate::helpers::emit_json;
use crate::TracedClient;
use andler_rpc::proto::{CloneInstanceRequest, ExportInstanceDiskRequest};

fn clone_json(source_instance_id: &str, instance_id: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance_id,
        "source_instance_id": source_instance_id,
    })
}

fn export_json(source_instance_id: &str, dest_path: &str) -> serde_json::Value {
    serde_json::json!({
        "dest_path": dest_path,
        "source_instance_id": source_instance_id,
    })
}

use crate::CliCloneMode;

pub async fn handle_clone(
    client: &mut TracedClient,
    source_instance_id: String,
    name: String,
    instances_root: String,
    mode: CliCloneMode,
    idempotency_token: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: source_instance_id.clone(),
            new_name: name,
            instances_root,
            mode: andler_rpc::proto::CloneMode::from(mode) as i32,
            idempotency_token,
        })
        .await?
        .into_inner();
    if json {
        emit_json(&clone_json(&source_instance_id, &response.instance_id))?;
    } else {
        println!(
            "cloned instance_id={}",
            crate::helpers::short_id(&response.instance_id)
        );
    }
    Ok(())
}

pub async fn handle_export(
    client: &mut TracedClient,
    source_instance_id: String,
    dest_path: String,
    idempotency_token: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .export_instance_disk(ExportInstanceDiskRequest {
            source_instance_id: source_instance_id.clone(),
            dest_path: dest_path.clone(),
            idempotency_token,
        })
        .await?
        .into_inner();
    if json {
        emit_json(&export_json(&source_instance_id, &response.dest_path))?;
    } else {
        println!("exported to {}", response.dest_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{clone_json, export_json};

    #[test]
    fn clone_json_serializes_instance_id() {
        let json = clone_json(
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        assert_eq!(json["instance_id"], "fedcba9876543210fedcba9876543210");
        assert_eq!(
            json["source_instance_id"],
            "0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn export_json_serializes_dest_path() {
        let json = export_json(
            "0123456789abcdef0123456789abcdef",
            "/data/export/image.qcow2",
        );
        assert_eq!(json["dest_path"], "/data/export/image.qcow2");
        assert_eq!(
            json["source_instance_id"],
            "0123456789abcdef0123456789abcdef"
        );
    }
}
