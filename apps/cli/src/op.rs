use crate::TracedClient;
use andler_rpc::proto::{Empty, OpCancelRequest};

use crate::OpAction;

pub async fn handle(
    client: &mut TracedClient,
    action: OpAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        OpAction::List {} => {
            let response = client.list_operations(Empty {}).await?.into_inner();
            if json {
                #[derive(serde::Serialize)]
                struct OpJson<'a> {
                    op_id: &'a str,
                    instance_id: &'a str,
                    kind: &'a str,
                    progress: f64,
                    state: &'a str,
                    error: &'a str,
                }
                let entries: Vec<OpJson> = response
                    .operations
                    .iter()
                    .map(|op| OpJson {
                        op_id: &op.op_id,
                        instance_id: &op.instance_id,
                        kind: &op.kind,
                        progress: op.progress,
                        state: &op.state,
                        error: &op.error,
                    })
                    .collect();
                println!("{}", serde_json::to_string(&entries)?);
                return Ok(());
            }
            if response.operations.is_empty() {
                println!("no active operations");
            } else {
                for op in &response.operations {
                    println!(
                        "op_id={}, instance_id={}, kind={}, state={}, progress={:.0}%, phase={}, error={}",
                        op.op_id,
                        op.instance_id,
                        op.kind,
                        op.state,
                        op.progress * 100.0,
                        op.phases
                            .iter()
                            .map(|p| p.name.clone())
                            .collect::<Vec<_>>()
                            .join(" -> "),
                        if op.error.is_empty() {
                            "-".to_string()
                        } else {
                            op.error.clone()
                        },
                    );
                }
            }
        }
        OpAction::Cancel { op_id } => {
            client
                .cancel_operation(OpCancelRequest {
                    op_id: op_id.clone(),
                })
                .await?;
            println!("operation {op_id} cancelled");
        }
    }
    Ok(())
}
