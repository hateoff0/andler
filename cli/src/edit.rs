

use andler_core::InstanceConfig;
use andler_rpc::convert;
use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::InstanceIdRequest;
use tonic::transport::Channel;

pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_instance_config(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?
        .into_inner();

    let resolved_ref = response.instance_id.clone();
    let original: InstanceConfig = response.try_into()?;
    let original_toml = toml::to_string_pretty(&original)
        .map_err(|e| format!("failed to serialize current config to TOML: {e}"))?;

    let temp_path = std::env::temp_dir().join(format!("andler-edit-{instance_id}.toml"));
    std::fs::write(&temp_path, &original_toml)?;

    let edit_result = run_editor(&temp_path);
    let edited_toml = std::fs::read_to_string(&temp_path);
    let _ = std::fs::remove_file(&temp_path);

    edit_result?;
    let edited_toml = edited_toml?;

    if edited_toml == original_toml {
        println!("No changes made.");
        return Ok(());
    }

    let edited: InstanceConfig = match toml::from_str(&edited_toml) {
        Ok(cfg) => cfg,
        Err(err) => {
            crate::err_exit(&format!(
                "Invalid TOML, no changes applied:\n{err}\n\
                 Run `andler edit {instance_id}` again to retry."
            ));
        }
    };

    let request = convert::instance_config_to_update_request(edited, resolved_ref);
    client.update_instance_config(request).await?;

    println!("Config updated. Restart instance to apply changes.");
    Ok(())
}


fn run_editor(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or("$VISUAL/$EDITOR is set but empty")?;
    let args: Vec<&str> = parts.collect();

    let status = std::process::Command::new(program)
        .args(&args)
        .arg(path)
        .status()
        .map_err(|e| format!("failed to launch editor `{program}`: {e}"))?;

    if !status.success() {
        return Err(format!("editor `{program}` exited with {status}").into());
    }
    Ok(())
}
