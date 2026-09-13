use andler_rpc::proto::{GuestProfileEntry, GuestProfileStatus, InstanceIdRequest};

use crate::helpers::spinner;
use crate::TracedClient;

use super::ui;
use super::{WizardError, WizardKind, WizardResult};

/// Creates the instance the wizard resolved and returns its full id.
pub async fn create(
    client: &mut TracedClient,
    result: &WizardResult,
) -> Result<String, WizardError> {
    let progress = spinner("Creating the VM…");
    let created = match result {
        WizardResult::Linux(req) => client
            .create_instance(req.clone())
            .await
            .map(|response| response.into_inner().instance_id),
        WizardResult::Android(req) => client
            .create_android_instance(req.clone())
            .await
            .map(|response| response.into_inner().instance_id),
    };
    progress.finish_and_clear();

    created.map_err(|e| WizardError::Inquire(crate::format_grpc_error(&e)))
}

/// Applies the guest-side selections the instance's own config asks for
/// (ARM translator, clipboard agent). Creation already succeeded, so a
/// failure here is reported per selection instead of unwinding the create.
pub async fn apply_guest_selections(
    client: &mut TracedClient,
    id: &str,
) -> Result<Vec<GuestProfileEntry>, WizardError> {
    let progress = spinner("Applying your selections inside the VM…");
    let mut call_client = client.clone();
    let result = crate::helpers::call_with_operation_progress(
        client,
        id,
        "applying the guest selections",
        |line| progress.set_message(line.to_string()),
        async move {
            call_client
                .apply_guest_profile(InstanceIdRequest {
                    instance_id: id.to_string(),
                })
                .await
        },
    )
    .await;
    progress.finish_and_clear();

    match result {
        Ok(response) => Ok(response.into_inner().entries),
        Err(e) => Err(WizardError::Inquire(crate::format_grpc_error(&e))),
    }
}

/// What the user is told after the wizard's work is done: the instance, the
/// selections that were applied (with a retry command for the ones that were
/// not), and the commands that come next. `None` means the caller chose not to
/// apply anything (`--quick`), which is reported with the command that does.
pub fn report(
    id: &str,
    kind: WizardKind,
    applied: Option<&Result<Vec<GuestProfileEntry>, WizardError>>,
) {
    let short = crate::helpers::short_id(id);
    println!();
    ui::success(&format!(
        "{} VM created: {short}",
        match kind {
            WizardKind::Linux => "Linux",
            WizardKind::Android => "Android",
        }
    ));

    match applied {
        None => {
            ui::note(
                "Selections were not installed (--quick): run `andler guest apply <id>` to \
                 install them now.",
            );
        }
        Some(Ok(entries)) if entries.is_empty() => {
            ui::note("No guest-side selections to apply.");
        }
        Some(Ok(entries)) => {
            for entry in entries {
                match entry.status() {
                    GuestProfileStatus::Applied => {
                        ui::success(&format!("{}: applied — {}", entry.name, entry.message))
                    }
                    GuestProfileStatus::AlreadyPresent => ui::note(&format!(
                        "{}: already present — {}",
                        entry.name, entry.message
                    )),
                    GuestProfileStatus::Skipped => {
                        ui::warn(&format!("{}: skipped — {}", entry.name, entry.message))
                    }
                    GuestProfileStatus::Failed => {
                        ui::failure(&format!("{}: failed — {}", entry.name, entry.message))
                    }
                    GuestProfileStatus::Unspecified => ui::warn(&format!(
                        "{}: unknown status — {}",
                        entry.name, entry.message
                    )),
                }
            }
        }
        Some(Err(e)) => ui::failure(&format!(
            "Could not apply the guest-side selections: {e}\n  \
             The VM itself was created; re-run `andler guest apply {short}` to retry."
        )),
    }

    println!();
    println!("Next steps:");
    println!("  andler start {short}       — boot it");
    println!("  andler connect {short}     — open the console/graphics session");
    println!("  andler config view {short} — inspect the resolved configuration");
}
