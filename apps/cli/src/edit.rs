use crate::helpers::{emit_json, which};
use crate::TracedClient;
use andler_core::InstanceConfig;
use andler_rpc::convert;
use andler_rpc::proto::InstanceIdRequest;
use serde::Serialize;

#[derive(Serialize)]
struct EditReport {
    instance_id: String,
    changed: bool,
    /// Top-level config sections that actually differ, e.g. `["cpu",
    /// "network"]`. Sourced from the same serialized comparison the
    /// `keypath` exhaustiveness test uses, not a second hand-rolled diff.
    changed_keys: Vec<String>,
}

pub async fn handle(
    client: &mut TracedClient,
    instance_id: String,
    json: bool,
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

    // Edit the real on-disk instance.toml directly — not a throwaway temp copy — so
    // the file you actually see in the instance directory is what you're editing,
    // and editing it by hand later does the same thing this command does. Assumes
    // `andler` runs on the same machine as `andlerd` (true for the common case;
    // if andlerd is remote, this won't find the file — the old temp-file behavior
    // didn't have that limitation, but silently editing a file nobody could see or
    // that did nothing when hand-edited was a worse default).
    let instance_dir = original
        .disk
        .path
        .parent()
        .ok_or("could not determine instance directory from disk path")?;
    let toml_path = instance_dir.join("instance.toml");

    // The file may not exist yet (old instances created before andler started
    // writing it at creation time, or if it was deleted) — (re)write the current
    // config there first so there's always something real on disk to edit.
    std::fs::write(&toml_path, &original_toml)
        .map_err(|e| format!("failed to write {}: {e}", toml_path.display()))?;

    run_editor(&toml_path)?;

    let edited_toml = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("failed to read back {}: {e}", toml_path.display()))?;

    if edited_toml == original_toml {
        if json {
            emit_json(&EditReport {
                instance_id: resolved_ref,
                changed: false,
                changed_keys: Vec::new(),
            })?;
        } else {
            println!("No changes made.");
        }
        return Ok(());
    }

    let edited: InstanceConfig = match toml::from_str(&edited_toml) {
        Ok(cfg) => cfg,
        Err(err) => {
            return Err(format!(
                "Invalid TOML — nothing was applied, but your edits are still saved in \
                 {}:\n{err}\n\
                 Fix it and run `andler config edit {instance_id}` again.",
                toml_path.display()
            )
            .into());
        }
    };

    let changed_keys = changed_top_level_keys(&original, &edited);

    let request = convert::instance_config_to_update_request(edited, resolved_ref.clone());
    client.update_instance_config(request).await?;

    if json {
        emit_json(&EditReport {
            instance_id: resolved_ref,
            changed: true,
            changed_keys,
        })?;
    } else {
        println!("Config updated. Restart instance to apply changes.");
    }
    Ok(())
}

/// Top-level fields that differ between two configs, by serialized value —
/// the same technique `keypath`'s exhaustiveness test uses to walk
/// `InstanceConfig`, applied here to compare instead of enumerate. Field
/// names are reported as-is (`cpu`, `network`, `name`, ...); nested
/// differences are not expanded further, since "which section changed" is
/// enough for a script deciding whether e.g. a restart is warranted.
fn changed_top_level_keys(before: &InstanceConfig, after: &InstanceConfig) -> Vec<String> {
    let (Ok(before), Ok(after)) = (serde_json::to_value(before), serde_json::to_value(after))
    else {
        return Vec::new();
    };
    let (Some(before), Some(after)) = (before.as_object(), after.as_object()) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = before
        .keys()
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn run_editor(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let explicit = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    let editor = match explicit {
        Some(e) => e,
        None => {
            // Neither $VISUAL nor $EDITOR is set. The old behavior hardcoded a
            // fallback to `vi` with no further check — if vi wasn't installed,
            // that failed with a bare "No such file or directory" and nothing
            // else to try. Instead, actually check what's available and use the
            // first editor found, only giving up if nothing at all is present.
            ["vi", "vim", "nano"]
                .iter()
                .find_map(|candidate| which(candidate).map(|_| candidate.to_string()))
                .ok_or(
                    "no editor found: $VISUAL and $EDITOR are both unset, and none of \
                     vi/vim/nano are installed. Set one, e.g. `export EDITOR=nano`, or \
                     install an editor.",
                )?
        }
    };

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
