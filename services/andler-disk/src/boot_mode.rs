use andler_core::{AndroidBootMode, GuestMutator, MutatorError, MutatorOp};

use crate::error::DiskError;
fn target_unit_path(mode: AndroidBootMode) -> &'static str {
    match mode {
        AndroidBootMode::Android => "/etc/systemd/system/android.target",
        AndroidBootMode::Linux => "/usr/lib/systemd/system/multi-user.target",
    }
}

/// Switches the guest's boot mode through a `GuestMutator` (offline:
/// `GuestfsMutator` on the stopped instance's disk; online: `QgaMutator`).
/// The default.target symlink is replaced (rm + ln), since `ln -s` alone
/// refuses to overwrite an existing link.
pub async fn switch_boot_mode_with(
    mutator: &dyn GuestMutator,
    mode: AndroidBootMode,
) -> Result<(), DiskError> {
    let target_unit = target_unit_path(mode);
    match mutator.read_file(target_unit).await {
        Ok(_) => {}
        Err(MutatorError::NotFound(_)) => {
            return Err(DiskError::FileSystem(format!(
                "{target_unit} not found in guest filesystem \u{2014} base image may predate boot mode switching"
            )));
        }
        Err(err) => {
            return Err(DiskError::FileSystem(format!(
                "failed to inspect {target_unit}: {err}"
            )));
        }
    }

    mutator
        .apply(&[
            MutatorOp::RmRf {
                path: "/etc/systemd/system/default.target".to_string(),
            },
            MutatorOp::Symlink {
                target: target_unit.to_string(),
                link: "/etc/systemd/system/default.target".to_string(),
            },
        ])
        .await
        .map_err(|err| {
            DiskError::FileSystem(format!("failed to write default.target symlink: {err}"))
        })?;

    tracing::info!(mode = ?mode, "android instance boot mode switched offline");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_unit_paths_match_systemd_layout() {
        assert_eq!(
            target_unit_path(AndroidBootMode::Android),
            "/etc/systemd/system/android.target"
        );
        assert_eq!(
            target_unit_path(AndroidBootMode::Linux),
            "/usr/lib/systemd/system/multi-user.target"
        );
    }
}
