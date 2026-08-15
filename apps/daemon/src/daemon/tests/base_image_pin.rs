use super::common::{sample_android_config, TestTempDir};
use super::*;
use andler_core::BaseImagePin;

fn write_fixture(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    let qcow2 = dir.join("img.qcow2");
    std::fs::write(&qcow2, b"fake qcow2 content that can be hashed").unwrap();
    let manifest = dir.join("img.manifest.json");
    std::fs::write(
        &manifest,
        r#"{"android_major": "13", "android_variant": "vanilla", "built_at": "2026-08-13T00:00:00Z"}"#,
    )
    .unwrap();
    let sha = andler_core::base_image::sha256_of(&qcow2).unwrap();
    (qcow2, sha)
}

#[test]
fn derive_records_id_and_sha256() {
    let dir = TestTempDir::new();
    let (qcow2, sha) = write_fixture(dir.path());

    let pin = Daemon::verify_or_derive_pin(&qcow2, None).unwrap().unwrap();

    assert_eq!(pin.id, "android13-vanilla-2026-08-13T00:00:00Z");
    assert_eq!(pin.sha256, sha);
    assert_eq!(pin.sha256.len(), 64);
}

#[test]
fn matching_pin_passes_and_returns_nothing_to_write() {
    let dir = TestTempDir::new();
    let (qcow2, sha) = write_fixture(dir.path());
    let pin = BaseImagePin {
        id: "android13-vanilla-2026-08-13T00:00:00Z".to_string(),
        sha256: sha.clone(),
    };

    let result = Daemon::verify_or_derive_pin(&qcow2, Some(&pin)).unwrap();
    assert!(result.is_none());
}

#[test]
fn mismatched_id_is_refused() {
    let dir = TestTempDir::new();
    let (qcow2, sha) = write_fixture(dir.path());
    let pin = BaseImagePin {
        id: "android11-vanilla-something-else".to_string(),
        sha256: sha,
    };

    let err = Daemon::verify_or_derive_pin(&qcow2, Some(&pin)).unwrap_err();
    assert!(
        matches!(err, DaemonError::BaseImagePinMismatch(_)),
        "err: {err}"
    );
    assert!(
        err.to_string().contains("swapped"),
        "err must call out the swap, got: {err}"
    );
}

#[test]
fn mismatched_sha256_is_refused() {
    let dir = TestTempDir::new();
    let (qcow2, _sha) = write_fixture(dir.path());
    let pin = BaseImagePin {
        id: "android13-vanilla-2026-08-13T00:00:00Z".to_string(),
        sha256: "0".repeat(64),
    };

    let err = Daemon::verify_or_derive_pin(&qcow2, Some(&pin)).unwrap_err();
    assert!(
        matches!(err, DaemonError::BaseImagePinMismatch(_)),
        "err: {err}"
    );
    assert!(
        err.to_string().contains("changed since it was pinned"),
        "err: {err}"
    );
}

#[test]
fn pin_without_manifest_is_refused_but_derivation_skips() {
    let dir = TestTempDir::new();
    let bare = dir.path().join("bare.qcow2");
    std::fs::write(&bare, b"x").unwrap();

    let pin = BaseImagePin {
        id: "android13-vanilla-whatever".to_string(),
        sha256: "0".repeat(64),
    };
    let err = Daemon::verify_or_derive_pin(&bare, Some(&pin)).unwrap_err();
    assert!(
        matches!(err, DaemonError::BaseImagePinMismatch(_)),
        "err: {err}"
    );

    let derived = Daemon::verify_or_derive_pin(&bare, None).unwrap();
    assert!(derived.is_none());
}

#[tokio::test]
async fn create_android_with_pin_mismatch_fails_before_touching_disk() {
    let dir = TestTempDir::new();
    let (qcow2, sha) = write_fixture(dir.path());
    let mut cfg = sample_android_config(dir.path().join("disk.qcow2"), qcow2.clone());
    let profile = match &mut cfg.kind {
        andler_core::InstanceKind::AndroidVm { android_profile } => {
            android_profile.base_image_pin = Some(BaseImagePin {
                id: "android13-vanilla-2026-08-13T00:00:00Z".to_string(),
                sha256: "0".repeat(64),
            });
            android_profile.clone()
        }
        other => panic!("expected android kind, got {other:?}"),
    };
    let _ = sha;

    let daemon = Daemon::new();
    let err = daemon
        .create_android_instance(
            profile,
            "pinned".to_string(),
            qcow2.clone(),
            dir.path().join("instances"),
            2 * 1024 * 1024 * 1024,
            dir.path().join("VARS.fd"),
            true,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DaemonError::BaseImagePinMismatch(_)),
        "err: {err}"
    );
    assert!(
        !dir.path().join("instances").exists(),
        "no instance dir may be created"
    );
}
