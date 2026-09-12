use super::common::*;
use super::*;
use andler_core::ArmTranslator;
use std::path::PathBuf;

/// An Android config that selected an ARM translator — the wizard's most
/// common non-empty selection list.
fn placeholder_android(disk: &str, base: &str) -> andler_core::InstanceConfig {
    let mut cfg = sample_android_config(PathBuf::from(disk), PathBuf::from(base));
    let andler_core::InstanceKind::AndroidVm { android_profile } = &mut cfg.kind else {
        panic!("sample_android_config must build an AndroidVm");
    };
    android_profile.arm_translator = ArmTranslator::Libndk;
    cfg
}

#[tokio::test]
async fn apply_guest_profile_on_unknown_instance_is_not_found() {
    let daemon = Daemon::new();

    let err = daemon
        .apply_guest_profile(InstanceId::new())
        .await
        .unwrap_err();

    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}

#[tokio::test]
async fn a_config_without_selections_applies_nothing() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.input.clipboard_enabled = false;
    let id = daemon.create_instance(cfg).await.unwrap();

    let outcomes = daemon.apply_guest_profile(id).await.unwrap();

    assert!(
        outcomes.is_empty(),
        "a Linux VM without clipboard sharing asks for nothing: {outcomes:?}"
    );
}

#[tokio::test]
async fn running_instance_skips_the_translator_with_a_stop_first_message() {
    // The translator is written to the instance disk, so it cannot be applied
    // to a live VM — the daemon must say so instead of attempting it.
    let daemon = Daemon::new();
    let cfg = placeholder_android(
        "/tmp/test-apply-running.qcow2",
        "/tmp/test-apply-base.qcow2",
    );
    let id = cfg.id;
    register_with_state(&daemon, cfg, InstanceState::Running, None).await;

    let outcomes = daemon.apply_guest_profile(id).await.unwrap();

    let translator = outcomes
        .iter()
        .find(|outcome| outcome.name == "arm-translator")
        .expect("the libndk selection is reported");
    assert_eq!(
        translator.status,
        andler_core::guest_profile::GuestSelectionStatus::Skipped
    );
    assert!(
        translator.message.contains("andler stop")
            && translator.message.contains("andler guest apply"),
        "the skip must be actionable: {}",
        translator.message
    );
}

#[tokio::test]
async fn disk_idle_apply_reports_every_selection_and_never_starts_the_vm() {
    // Placeholder disk (no guest filesystem): each selection must end in a
    // classified outcome with the retry command, and applying a profile on a
    // Created instance must leave it Created — the offline appliance path is
    // the only one allowed here, never the auto-start maintenance path.
    let daemon = Daemon::new();
    let mut cfg = placeholder_android(
        "/tmp/test-apply-idle.qcow2",
        "/tmp/test-apply-idle-base.qcow2",
    );
    cfg.input.clipboard_enabled = true;
    let id = daemon.create_instance(cfg).await.unwrap();

    let outcomes = daemon.apply_guest_profile(id).await.unwrap();

    let names: Vec<&str> = outcomes.iter().map(|outcome| outcome.name).collect();
    assert_eq!(names, vec!["arm-translator", "spice-vdagent"]);
    for outcome in &outcomes {
        assert!(
            !outcome.message.is_empty(),
            "every outcome explains itself: {outcome:?}"
        );
        assert_ne!(
            outcome.status,
            andler_core::guest_profile::GuestSelectionStatus::Applied,
            "a disk without a guest filesystem cannot install anything: {outcome:?}"
        );
    }
    let translator = &outcomes[0];
    assert!(
        translator.message.contains("andler guest install libndk"),
        "a failed translator install carries its retry command: {}",
        translator.message
    );
    let state = daemon.handle_for(id).await.unwrap().state();
    assert_eq!(
        state,
        InstanceState::Created,
        "applying a profile must not boot the VM"
    );
}
