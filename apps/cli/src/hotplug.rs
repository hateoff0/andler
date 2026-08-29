use crate::TracedClient;
use andler_rpc::proto::{
    AttachDiskRequest, AttachNetworkRequest, DetachDiskRequest, DetachNetworkRequest,
    NetworkConfig as ProtoNetworkConfig, NetworkMode as ProtoNetworkMode,
};

use crate::{AttachAction, CliAttachNetMode, CliNatBackend, DetachAction};

fn proto_network_config(
    mode: CliAttachNetMode,
    bridge: Option<String>,
    model: String,
    nat_backend: CliNatBackend,
) -> ProtoNetworkConfig {
    let kind = match mode {
        CliAttachNetMode::Nat => {
            andler_rpc::proto::network_mode::Kind::Nat(andler_rpc::proto::network_mode::Nat {})
        }
        CliAttachNetMode::Bridge => {
            andler_rpc::proto::network_mode::Kind::Bridge(andler_rpc::proto::network_mode::Bridge {
                interface: bridge.unwrap_or_default(),
            })
        }
        CliAttachNetMode::Isolated => andler_rpc::proto::network_mode::Kind::Isolated(
            andler_rpc::proto::network_mode::Isolated {},
        ),
    };
    let mut msg = ProtoNetworkConfig {
        mode: Some(ProtoNetworkMode { kind: Some(kind) }),
        device_model: model,
        ..Default::default()
    };
    msg.set_nat_backend(match nat_backend {
        CliNatBackend::Slirp => andler_rpc::proto::NatBackend::Slirp,
        CliNatBackend::Passt => andler_rpc::proto::NatBackend::Passt,
    });
    msg
}

pub async fn handle_attach(
    client: &mut TracedClient,
    action: AttachAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        AttachAction::Disk {
            instance_id,
            path,
            size,
        } => {
            let needs_creation = path.as_ref().is_none_or(|p| !p.exists());
            if needs_creation && size.is_none() {
                return Err(
                    "attach disk: --size is required when the disk image does not exist yet".into(),
                );
            }
            let response = client
                .attach_disk(AttachDiskRequest {
                    instance_id: instance_id.clone(),
                    path: path
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size_bytes: size.unwrap_or(0),
                })
                .await?
                .into_inner();
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "attach_disk",
                        "instance_id": instance_id,
                        "path": response.path,
                        "index": response.index,
                    }))?
                );
            } else {
                println!(
                    "disk attached: {} (extra disk index {})",
                    response.path, response.index
                );
            }
        }
        AttachAction::Net {
            instance_id,
            mode,
            bridge,
            model,
            nat_backend,
        } => {
            let response = client
                .attach_network(AttachNetworkRequest {
                    instance_id: instance_id.clone(),
                    network: Some(proto_network_config(mode, bridge, model, nat_backend)),
                })
                .await?
                .into_inner();
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "attach_network",
                        "instance_id": instance_id,
                        "index": response.index,
                    }))?
                );
            } else {
                println!("network attached: extra network index {}", response.index);
            }
        }
    }
    Ok(())
}

pub async fn handle_detach(
    client: &mut TracedClient,
    action: DetachAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DetachAction::Disk { instance_id, path } => {
            client
                .detach_disk(DetachDiskRequest {
                    instance_id: instance_id.clone(),
                    path: path.to_string_lossy().into_owned(),
                })
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "detach_disk",
                        "instance_id": instance_id,
                        "path": path,
                    }))?
                );
            } else {
                println!("disk detached (the image file was kept)");
            }
        }
        DetachAction::Net { instance_id, index } => {
            client
                .detach_network(DetachNetworkRequest {
                    instance_id: instance_id.clone(),
                    index: index as u32,
                })
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "detach_network",
                        "instance_id": instance_id,
                        "index": index,
                    }))?
                );
            } else {
                println!("network detached (extra network index {index})");
            }
        }
    }
    Ok(())
}
