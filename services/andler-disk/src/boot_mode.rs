use andler_core::{AndroidBootMode, GuestMutator, MutatorError, MutatorOp};

use crate::error::DiskError;
const DEFAULT_TARGET: &str = "/etc/systemd/system/default.target";

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
                path: DEFAULT_TARGET.to_string(),
            },
            MutatorOp::Symlink {
                target: target_unit.to_string(),
                link: DEFAULT_TARGET.to_string(),
            },
        ])
        .await
        .map_err(|err| {
            DiskError::FileSystem(format!("failed to write default.target symlink: {err}"))
        })?;

    // `apply` returning Ok means the appliance took the batch, not that the
    // guest filesystem will still have it: the session is torn down by killing
    // the appliance, which holds the image with a write-back cache, so a batch
    // that is not flushed can vanish — and this switch is a single small write
    // at the end of a short session, exactly the shape that loses that race.
    // Read it back, so a lost write fails here instead of surfacing hours later
    // as "the instance says android but booted Linux".
    let applied = mutator.read_file(DEFAULT_TARGET).await.map_err(|err| {
        DiskError::FileSystem(format!("could not read back {DEFAULT_TARGET}: {err}"))
    })?;
    let expected = mutator
        .read_file(target_unit)
        .await
        .map_err(|err| DiskError::FileSystem(format!("could not read {target_unit}: {err}")))?;
    if applied != expected {
        return Err(DiskError::FileSystem(format!(
            "{DEFAULT_TARGET} does not follow {target_unit} after the switch: the write did not reach the guest filesystem"
        )));
    }

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
