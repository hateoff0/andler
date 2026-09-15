use super::common::*;
use super::*;

#[tokio::test]
async fn full_id_resolves_without_needing_registration() {
    let daemon = Daemon::new();
    let random_id = InstanceId::new();

    let resolved = daemon
        .resolve_instance_id(&random_id.to_string())
        .await
        .expect("full ID must resolve");

    assert_eq!(resolved, random_id);
}

#[tokio::test]
async fn empty_reference_is_rejected() {
    let daemon = Daemon::new();
    let err = daemon.resolve_instance_id("").await.unwrap_err();
    assert!(matches!(err, DaemonError::EmptyInstanceRef));
}

#[tokio::test]
async fn unmatched_prefix_is_not_found() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.id = InstanceId::new();
    daemon.create_instance(cfg).await.expect("create instance");

    let err = daemon.resolve_instance_id("ffffffff").await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceRefNotFound(prefix) if prefix == "ffffffff"));
}

#[tokio::test]
async fn unique_prefix_resolves_to_the_matching_instance() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.id = InstanceId::new();
    let id = cfg.id;
    daemon.create_instance(cfg).await.expect("create instance");

    let full = id.to_string();
    let prefix = &full[..12];

    let resolved = daemon
        .resolve_instance_id(prefix)
        .await
        .expect("unique prefix must resolve");
    assert_eq!(resolved, id);

    let resolved_upper = daemon
        .resolve_instance_id(&prefix.to_ascii_uppercase())
        .await
        .expect("prefix must resolve case-insensitively");
    assert_eq!(resolved_upper, id);
}

#[tokio::test]
async fn ambiguous_prefix_lists_every_candidate() {
    let daemon = Daemon::new();

    let id_a = "a".repeat(64).parse::<InstanceId>().expect("valid hex");
    let id_b = format!("{}bb", "a".repeat(62))
        .parse::<InstanceId>()
        .expect("valid hex");
    assert_ne!(id_a, id_b);

    let mut cfg_a = sample_config();
    cfg_a.id = id_a;
    daemon.create_instance(cfg_a).await.expect("create a");

    let mut cfg_b = sample_config();
    cfg_b.id = id_b;
    daemon.create_instance(cfg_b).await.expect("create b");

    let prefix = "a".repeat(20);
    let err = daemon.resolve_instance_id(&prefix).await.unwrap_err();

    match err {
        DaemonError::AmbiguousInstanceId { prefix, candidates } => {
            assert_eq!(prefix, "a".repeat(20));
            assert_eq!(candidates.len(), 2);
            assert!(candidates.contains(&id_a));
            assert!(candidates.contains(&id_b));
        }
        other => panic!("expected AmbiguousInstanceId, got {other:?}"),
    }
}

#[tokio::test]
async fn an_ambiguous_prefix_is_reported_with_readable_ids() {
    let daemon = Daemon::new();
    let id_a = "a".repeat(64).parse::<InstanceId>().expect("valid hex");
    let id_b = format!("{}bb", "a".repeat(62))
        .parse::<InstanceId>()
        .expect("valid hex");
    for id in [id_a, id_b] {
        let mut cfg = sample_config();
        cfg.id = id;
        daemon.create_instance(cfg).await.expect("create");
    }

    let message = daemon
        .resolve_instance_id(&"a".repeat(20))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        message.contains(&"a".repeat(64)) && message.contains(&format!("{}bb", "a".repeat(62))),
        "the operator has to be able to copy an id out of this message: {message}"
    );
    assert!(
        !message.contains("InstanceId(["),
        "raw byte arrays are not an id an operator can use: {message}"
    );
}
