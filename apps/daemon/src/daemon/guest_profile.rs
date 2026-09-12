use andler_core::guest_profile::{
    guest_selections, GuestSelection, GuestSelectionKind, GuestSelectionOutcome,
    GuestSelectionStatus,
};
use andler_core::{InstanceConfig, InstanceId, InstanceState};
use andler_disk::arm_translator::TranslatorSwitch;
use andler_disk::DiskError;

use super::error::DaemonError;
use super::Daemon;

impl Daemon {
    /// Applies every guest-side selection the instance's own configuration
    /// asks for (see `andler_core::guest_profile::guest_selections`) and
    /// reports each one. Creation never waits on this: the wizard calls it
    /// right after `create`, and `andler guest apply` re-runs it later.
    ///
    /// One selection failing does not abort the rest — a missing network for
    /// the translator download must not also cost the clipboard agent — so
    /// the caller gets per-selection outcomes rather than a single error.
    pub async fn apply_guest_profile(
        &self,
        id: InstanceId,
    ) -> Result<Vec<GuestSelectionOutcome>, DaemonError> {
        let handle = self.handle_for(id).await?;
        let state = handle.state();
        let cfg = handle.config();
        let selections = guest_selections(&cfg);

        let mut outcomes = Vec::with_capacity(selections.len());
        for selection in &selections {
            outcomes.push(
                self.apply_selection(id, &cfg, state.clone(), selection)
                    .await,
            );
            // A state change mid-apply (someone started the VM) invalidates
            // the snapshot the remaining selections were classified with.
            if handle.state() != state {
                outcomes.push(GuestSelectionOutcome::new(
                    selection.name,
                    GuestSelectionStatus::Skipped,
                    format!(
                        "the instance state changed to {:?} while selections were being applied; \
                         re-run `andler guest apply {id}` once it is settled",
                        handle.state()
                    ),
                ));
                break;
            }
        }

        Ok(outcomes)
    }

    async fn apply_selection(
        &self,
        id: InstanceId,
        cfg: &InstanceConfig,
        state: InstanceState,
        selection: &GuestSelection,
    ) -> GuestSelectionOutcome {
        match &selection.kind {
            GuestSelectionKind::ArmTranslator(translator) => {
                if !state.is_disk_idle() {
                    return GuestSelectionOutcome::new(
                        selection.name,
                        GuestSelectionStatus::Skipped,
                        format!(
                            "the ARM translator is written to the instance disk, so it needs the \
                             VM stopped (currently {state:?}): `andler stop {id}` and re-run \
                             `andler guest apply {id}`"
                        ),
                    );
                }

                match self.switch_arm_translator(id, *translator, None).await {
                    Ok(TranslatorSwitch::Installed) => GuestSelectionOutcome::new(
                        selection.name,
                        GuestSelectionStatus::Applied,
                        format!("{translator} installed into {}", cfg.disk.path.display()),
                    ),
                    Ok(TranslatorSwitch::AlreadyInstalled) => GuestSelectionOutcome::new(
                        selection.name,
                        GuestSelectionStatus::AlreadyPresent,
                        format!("{translator} is already installed"),
                    ),
                    Err(e) => GuestSelectionOutcome::failure(
                        selection.name,
                        &short_id(id),
                        selection,
                        e.to_string(),
                    ),
                }
            }
            GuestSelectionKind::Package(package) => {
                // Disk-idle instances install offline through the libguestfs
                // appliance; running ones install in-place via the guest
                // agent. Never the auto-start maintenance path: applying a
                // profile must not boot a VM the user has not started.
                let offline = state.is_disk_idle();
                let result = self
                    .install_guest_agent(id, (*package).to_string(), offline, None)
                    .await;

                match result {
                    Ok(()) => GuestSelectionOutcome::new(
                        selection.name,
                        GuestSelectionStatus::Applied,
                        format!(
                            "{package} installed ({})",
                            if offline { "offline" } else { "online" }
                        ),
                    ),
                    Err(DaemonError::Disk(DiskError::AgentAlreadyInstalled { .. })) => {
                        GuestSelectionOutcome::new(
                            selection.name,
                            GuestSelectionStatus::AlreadyPresent,
                            format!("{package} is already installed"),
                        )
                    }
                    Err(DaemonError::Disk(DiskError::NoGuestOs { .. }))
                    | Err(DaemonError::Disk(DiskError::PackageManagerNotFound { .. })) => {
                        GuestSelectionOutcome::new(
                            selection.name,
                            GuestSelectionStatus::Skipped,
                            format!(
                                "no installed guest OS found on the disk yet, so there is nothing \
                                 to install {package} into — install the OS in the VM first, then \
                                 run `andler guest install {package} {id}`"
                            ),
                        )
                    }
                    Err(e) => GuestSelectionOutcome::failure(
                        selection.name,
                        &short_id(id),
                        selection,
                        e.to_string(),
                    ),
                }
            }
        }
    }
}

fn short_id(id: InstanceId) -> String {
    id.to_string().chars().take(8).collect()
}
