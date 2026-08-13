use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::ExecCommandRequest;
use tonic::transport::Channel;

/// `andler exec <id> -- cmd args...` — runs a command in the guest through
/// the guest agent and relays its exit code. Programmatic access level of
/// `andler connect`; works without any guest network setup.
pub async fn handle_exec(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    argv: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .exec_command(ExecCommandRequest {
            instance_id,
            argv,
            timeout_secs: None,
        })
        .await?
        .into_inner();

    if !response.stdout.is_empty() {
        print!("{}", response.stdout);
    }
    if !response.stderr.is_empty() {
        eprint!("{}", response.stderr);
    }
    std::process::exit(response.exit_code);
}
