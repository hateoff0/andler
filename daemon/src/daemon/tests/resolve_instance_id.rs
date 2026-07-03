use super::common::*;
use super::*;

#[tokio::test]
async fn full_uuid_resolves_without_needing_registration() {
    // A syntactically valid, full UUID is accepted even if no instance
    // with that id has been registered yet — see the doc comment on
    // `resolve_instance_id` for why this is intentional (callers decide
    // whether "well-formed but unregistered" is itself an error, at the
    // point they actually use the id, e.g. `Daemon::status`).
    let daemon = Daemon::new();
    let random_id = InstanceId::new();

    let resolved = daemon
        .resolve_instance_id(&random_id.0.to_string())
        .await
        .expect("full UUID must resolve");

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

    let err = daemon
        .resolve_instance_id("ffffffff")
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::InstanceRefNotFound(prefix) if prefix == "ffffffff"));
}

#[tokio::test]
async fn unique_prefix_resolves_to_the_matching_instance() {
    let daemon = Daemon::new();
    let mut cfg = sample_config();
    cfg.id = InstanceId::new();
    let id = cfg.id;
    daemon.create_instance(cfg).await.expect("create instance");

    let full = id.0.to_string();
    // Docker-style short id: first 8 hex chars of the hyphenated string.
    let prefix = &full[..8];

    let resolved = daemon
        .resolve_instance_id(prefix)
        .await
        .expect("unique prefix must resolve");
    assert_eq!(resolved, id);

    // Case-insensitive, since users may paste an upper-cased id.
    let resolved_upper = daemon
        .resolve_instance_id(&prefix.to_ascii_uppercase())
        .await
        .expect("prefix must resolve case-insensitively");
    assert_eq!(resolved_upper, id);
}

#[tokio::test]
async fn ambiguous_prefix_lists_every_candidate() {
    let daemon = Daemon::new();

    // Force a shared prefix by constructing two ids that start with the
    // same fixed byte pattern (rather than looping on real UUIDs and
    // hoping for a collision).
    let shared_prefix_hex = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let id_a = InstanceId(uuid::Uuid::parse_str(shared_prefix_hex).unwrap());
    let shared_prefix_hex_b = "aaaaaaaa-aaaa-4aaa-8aaa-bbbbbbbbbbbb";
    let id_b = InstanceId(uuid::Uuid::parse_str(shared_prefix_hex_b).unwrap());

    let mut cfg_a = sample_config();
    cfg_a.id = id_a;
    daemon.create_instance(cfg_a).await.expect("create a");

    let mut cfg_b = sample_config();
    cfg_b.id = id_b;
    daemon.create_instance(cfg_b).await.expect("create b");

    let err = daemon
        .resolve_instance_id("aaaaaaaa-aaaa-4aaa-8aaa-")
        .await
        .unwrap_err();

    match err {
        DaemonError::AmbiguousInstanceId { prefix, candidates } => {
            assert_eq!(prefix, "aaaaaaaa-aaaa-4aaa-8aaa-");
            assert_eq!(candidates.len(), 2);
            assert!(candidates.contains(&id_a));
            assert!(candidates.contains(&id_b));
        }
        other => panic!("expected AmbiguousInstanceId, got {other:?}"),
    }
}
