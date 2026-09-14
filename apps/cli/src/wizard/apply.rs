use andler_rpc::proto::{GuestProfileEntry, GuestProfileStatus, InstanceIdRequest};

use crate::TracedClient;

use super::ui::{self, Progress, Screen, Status};
use super::{WizardError, WizardKind, WizardResult};

pub async fn create(
    client: &mut TracedClient,
    result: &WizardResult,
) -> Result<String, WizardError> {
    let mut progress = Progress::start("creating the vm");
    let created = match result.creation.clone().into_request() {
        crate::create::ProtoRequest::Linux(req) => client
            .create_instance(*req)
            .await
            .map(|response| response.into_inner().instance_id),
        crate::create::ProtoRequest::Android(req) => client
            .create_android_instance(*req)
            .await
            .map(|response| response.into_inner().instance_id),
    };
    progress.finish();

    created.map_err(|e| WizardError::Inquire(crate::format_grpc_error(&e)))
}

pub async fn apply_guest_selections(
    client: &mut TracedClient,
    id: &str,
) -> Result<Vec<GuestProfileEntry>, WizardError> {
    let mut progress = Progress::start("applying your selections inside the vm");
    let mut call_client = client.clone();
    let result = crate::helpers::call_with_operation_progress(
        client,
        id,
        "applying the guest selections",
        |line| progress.update(line),
        async move {
            call_client
                .apply_guest_profile(InstanceIdRequest {
                    instance_id: id.to_string(),
                })
                .await
        },
    )
    .await;
    progress.finish();

    match result {
        Ok(response) => Ok(response.into_inner().entries),
        Err(e) => Err(WizardError::Inquire(crate::format_grpc_error(&e))),
    }
}

pub fn report(
    id: &str,
    kind: WizardKind,
    applied: Option<&Result<Vec<GuestProfileEntry>, WizardError>>,
) {
    let short = crate::helpers::short_id(id);
    ui::header(&format!(
        "{} vm created {short}",
        match kind {
            WizardKind::Linux => "linux",
            WizardKind::Android => "android",
        }
    ));

    let mut screen = Screen::new();
    screen.section("installed in the guest");
    match applied {
        None => {
            screen.note(
                "Selections were not installed (--quick): run `andler guest apply <id>` to \
                 install them now.",
            );
        }
        Some(Ok(entries)) if entries.is_empty() => {
            screen.note("No guest-side selections to apply.");
        }
        Some(Ok(entries)) => {
            for entry in entries {
                screen.entry(
                    &entry.name,
                    selection_status(entry.status()),
                    entry.message.clone(),
                );
            }
        }
        Some(Err(e)) => {
            screen.outcome(
                Status::Failed,
                format!("Could not apply the guest-side selections: {e}"),
            );
            screen.note(&format!(
                "The VM itself was created; re-run `andler guest apply {short}` to retry."
            ));
        }
    }

    screen.section("next steps");
    screen.field("start", format!("andler start {short} — boot it"));
    screen.field(
        "connect",
        format!("andler connect {short} — open the console/graphics session"),
    );
    screen.field(
        "config view",
        format!("andler config view {short} — inspect the resolved configuration"),
    );
    screen.print();
}

fn selection_status(status: GuestProfileStatus) -> Status {
    match status {
        GuestProfileStatus::Applied => Status::Ok,
        GuestProfileStatus::AlreadyPresent => Status::Present,
        GuestProfileStatus::Skipped => Status::Skipped,
        GuestProfileStatus::Failed => Status::Failed,
        GuestProfileStatus::Unspecified => Status::Unknown,
    }
}
