use crate::proto;
use andler_core::guest_profile::{GuestSelectionOutcome, GuestSelectionStatus};

pub fn status_to_proto(status: GuestSelectionStatus) -> proto::GuestProfileStatus {
    match status {
        GuestSelectionStatus::Applied => proto::GuestProfileStatus::Applied,
        GuestSelectionStatus::AlreadyPresent => proto::GuestProfileStatus::AlreadyPresent,
        GuestSelectionStatus::Skipped => proto::GuestProfileStatus::Skipped,
        GuestSelectionStatus::Failed => proto::GuestProfileStatus::Failed,
    }
}

pub fn apply_profile_response(
    outcomes: &[GuestSelectionOutcome],
) -> proto::ApplyGuestProfileResponse {
    proto::ApplyGuestProfileResponse {
        entries: outcomes
            .iter()
            .map(|outcome| proto::GuestProfileEntry {
                name: outcome.name.to_string(),
                status: status_to_proto(outcome.status).into(),
                message: outcome.message.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_maps_to_its_own_proto_value() {
        let mapped: Vec<proto::GuestProfileStatus> = [
            GuestSelectionStatus::Applied,
            GuestSelectionStatus::AlreadyPresent,
            GuestSelectionStatus::Skipped,
            GuestSelectionStatus::Failed,
        ]
        .into_iter()
        .map(status_to_proto)
        .collect();

        assert_eq!(mapped[0], proto::GuestProfileStatus::Applied);
        assert_eq!(mapped[1], proto::GuestProfileStatus::AlreadyPresent);
        assert_eq!(mapped[2], proto::GuestProfileStatus::Skipped);
        assert_eq!(mapped[3], proto::GuestProfileStatus::Failed);
    }

    #[test]
    fn response_carries_name_status_and_message_per_entry() {
        let outcomes = vec![
            GuestSelectionOutcome::new(
                "arm-translator",
                GuestSelectionStatus::Applied,
                "installed libndk".to_string(),
            ),
            GuestSelectionOutcome::new(
                "spice-vdagent",
                GuestSelectionStatus::Skipped,
                "no guest OS yet".to_string(),
            ),
        ];

        let response = apply_profile_response(&outcomes);

        assert_eq!(response.entries.len(), 2);
        assert_eq!(response.entries[0].name, "arm-translator");
        assert_eq!(
            response.entries[0].status,
            proto::GuestProfileStatus::Applied as i32
        );
        assert_eq!(response.entries[0].message, "installed libndk");
        assert_eq!(
            response.entries[1].status,
            proto::GuestProfileStatus::Skipped as i32
        );
    }
}
