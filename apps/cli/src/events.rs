use crate::TracedClient;
use andler_rpc::proto::EventStreamRequest;

/// `andler events [<id>] [--follow] [--json]` — streams daemon events
/// (lifecycle transitions, operations, QMP events) from the live bus.
/// Without `--follow` it exits after the first event, which makes the
/// command scriptable in tests.
pub async fn handle(
    client: &mut TracedClient,
    instance_id: Option<String>,
    follow: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = client
        .stream_events(EventStreamRequest {
            instance_id: instance_id.unwrap_or_default(),
        })
        .await?
        .into_inner();

    loop {
        let Some(message) = stream.message().await? else {
            break;
        };
        if json {
            println!(
                "{{\"ts_ms\":{},\"instance_id\":\"{}\",\"kind\":\"{}\",\"detail\":{}}}",
                message.ts_ms,
                message.instance_id,
                message.kind,
                if message.detail.is_empty() {
                    "{}".to_string()
                } else {
                    message.detail
                }
            );
        } else {
            println!(
                "{} [{}] {}",
                message.ts_ms,
                if message.instance_id.is_empty() {
                    "-".to_string()
                } else {
                    message.instance_id
                },
                message.detail
            );
        }
        if !follow {
            break;
        }
    }
    Ok(())
}
