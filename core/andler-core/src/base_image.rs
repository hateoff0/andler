use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

use crate::android_profile::AndroidProfile;
use crate::paths::base_images_dir;

#[derive(Debug, Deserialize)]
struct Manifest {
    android_major: String,
    android_variant: String,
    built_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseImageInfo {
    pub qcow2_path: PathBuf,
    pub manifest_path: PathBuf,
    pub android_major: String,
    pub android_variant: String,
    pub built_at: String,
}

impl BaseImageInfo {
    /// Stable id of this image build: the manifest fields that uniquely
    /// identify what the image contains. The qcow2 content can change
    /// without the id changing (a rebuild with the same label), which is
    /// exactly why the pin pairs the id with a content sha256.
    pub fn id(&self) -> String {
        format!(
            "android{}-{}-{}",
            self.android_major,
            self.android_variant.to_lowercase(),
            self.built_at
        )
    }
}

/// Reads the manifest describing the image at `qcow2_path` (a
/// `<stem>.manifest.json` next to it), if present.
pub fn info_for(qcow2_path: &std::path::Path) -> Option<BaseImageInfo> {
    let manifest_path = qcow2_path.with_extension("manifest.json");
    let file_name = manifest_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".manifest.json")?;
    let raw = fs::read_to_string(&manifest_path).ok()?;
    let manifest: Manifest = serde_json::from_str(&raw).ok()?;
    let qcow2 = manifest_path.with_file_name(format!("{stem}.qcow2"));
    if qcow2 != qcow2_path {
        return None;
    }
    Some(BaseImageInfo {
        qcow2_path: qcow2,
        manifest_path,
        android_major: manifest.android_major,
        android_variant: manifest.android_variant,
        built_at: manifest.built_at,
    })
}

/// sha256 of a file, hex-encoded. Streamed with a fixed buffer so large
/// base images never load into memory.
pub fn sha256_of(path: &std::path::Path) -> Result<String, BaseImageError> {
    use sha2::Digest;
    let mut file = fs::File::open(path).map_err(|source| BaseImageError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf).map_err(|source| {
            BaseImageError::ReadFile {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

#[derive(Debug, Error)]
pub enum BaseImageError {
    #[error(
        "no base image found for Android {android_major} ({variant}) in {searched:?}. \
         Build one first: docker/images/build.sh {android_major} {variant}"
    )]
    NotFound {
        android_major: String,
        variant: String,
        searched: PathBuf,
    },

    #[error("cannot read base image directory {path:?}: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot read base image file {path:?}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl AndroidProfile {
    pub fn variant_label(&self) -> &'static str {
        if self.gapps {
            "GAPPS"
        } else {
            "VANILLA"
        }
    }
}

pub fn list_matching(profile: &AndroidProfile) -> Result<Vec<BaseImageInfo>, BaseImageError> {
    let wanted_major = profile.android_version.to_string();
    let wanted_variant = profile.variant_label();

    let mut found = Vec::new();
    for info in collect_images()? {
        if info.android_major == wanted_major && info.android_variant == wanted_variant {
            found.push(info);
        }
    }

    // ISO-8601 UTC timestamps sort lexicographically in chronological order.
    found.sort_by(|a, b| b.built_at.cmp(&a.built_at));
    Ok(found)
}

/// Every valid base image in the cache (all versions/variants), for tooling
/// like `andler doctor` that reports what is available.
pub fn list_all() -> Result<Vec<BaseImageInfo>, BaseImageError> {
    collect_images()
}

/// Parses every `*.manifest.json` with a matching qcow2 next to it, regardless
/// of the requested profile.
fn collect_images() -> Result<Vec<BaseImageInfo>, BaseImageError> {
    let mut found = Vec::new();
    for manifest_path in collect_manifest_paths()? {
        let Some(file_name) = manifest_path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".manifest.json") else {
            continue;
        };
        let raw = match fs::read_to_string(&manifest_path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let manifest: Manifest = match serde_json::from_str(&raw) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        let qcow2_path = manifest_path.with_file_name(format!("{stem}.qcow2"));
        if !qcow2_path.exists() {
            continue;
        }
        found.push(BaseImageInfo {
            qcow2_path,
            manifest_path,
            android_major: manifest.android_major,
            android_variant: manifest.android_variant,
            built_at: manifest.built_at,
        });
    }
    Ok(found)
}

/// `*.manifest.json` files in the cache root and in one-level subdirectories
/// (e.g. `cache/base-images/android13-vanilla/`). The flat root is kept working
/// so images built before subdirectories existed are still discovered; never
/// recurse deeper than one level — build outputs land exactly one level down.
fn collect_manifest_paths() -> Result<Vec<PathBuf>, BaseImageError> {
    let dir = base_images_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(BaseImageError::ReadDir { path: dir, source }),
    };

    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        if name.ends_with(".manifest.json") {
            manifests.push(path);
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let Ok(sub_entries) = fs::read_dir(&path) else {
            continue;
        };
        for sub_entry in sub_entries.flatten() {
            let sub_path = sub_entry.path();
            if sub_path
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|n| n.ends_with(".manifest.json"))
            {
                manifests.push(sub_path);
            }
        }
    }
    Ok(manifests)
}

pub fn resolve(profile: &AndroidProfile) -> Result<PathBuf, BaseImageError> {
    list_matching(profile)?
        .into_iter()
        .next()
        .map(|info| info.qcow2_path)
        .ok_or_else(|| BaseImageError::NotFound {
            android_major: profile.android_version.to_string(),
            variant: profile.variant_label().to_string(),
            searched: base_images_dir(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_profile::{AndroidBootMode, AndroidVersion, ArmTranslator};
    use crate::config::InstanceId;
    use crate::paths::ANDLER_HOME_ENV;
    use crate::test_lock::ENV_LOCK;

    fn profile(version: AndroidVersion, gapps: bool) -> AndroidProfile {
        AndroidProfile {
            android_version: version,
            gapps,
            microg: false,
            arm_translator: ArmTranslator::None,
            boot_mode: AndroidBootMode::Android,
            base_image_pin: None,
        }
    }

    fn write_manifest(
        dir: &std::path::Path,
        name: &str,
        major: &str,
        variant: &str,
        built_at: &str,
    ) {
        fs::write(dir.join(format!("{name}.qcow2")), b"placeholder").unwrap();
        fs::write(
            dir.join(format!("{name}.manifest.json")),
            format!(
                r#"{{"schema_version":1,"android_major":"{major}","android_variant":"{variant}","built_at":"{built_at}"}}"#
            ),
        )
        .unwrap();
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        base: PathBuf,
    }

    impl EnvGuard {
        fn new() -> (Self, PathBuf) {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let base =
                std::env::temp_dir().join(format!("andler-base-image-test-{}", InstanceId::new()));
            let cache_dir = base.join("cache").join("base-images");
            fs::create_dir_all(&cache_dir).unwrap();
            std::env::set_var(ANDLER_HOME_ENV, &base);
            (Self { _lock: lock, base }, cache_dir)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(ANDLER_HOME_ENV);
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn resolve_finds_no_match_when_directory_missing() {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            ANDLER_HOME_ENV,
            "/tmp/andler-base-image-test-nonexistent-home",
        );
        let err = resolve(&profile(AndroidVersion::Android13, false)).unwrap_err();
        assert!(matches!(err, BaseImageError::NotFound { .. }));
        std::env::remove_var(ANDLER_HOME_ENV);
        drop(lock);
    }

    #[test]
    fn resolve_picks_freshest_matching_manifest() {
        let (_guard, dir) = EnvGuard::new();
        write_manifest(&dir, "old", "13", "VANILLA", "2026-01-01T00:00:00Z");
        write_manifest(&dir, "new", "13", "VANILLA", "2026-06-01T00:00:00Z");
        write_manifest(&dir, "other-major", "11", "VANILLA", "2026-12-01T00:00:00Z");
        write_manifest(&dir, "other-variant", "13", "GAPPS", "2026-12-01T00:00:00Z");

        let resolved = resolve(&profile(AndroidVersion::Android13, false)).unwrap();
        assert_eq!(resolved, dir.join("new.qcow2"));
    }

    #[test]
    fn resolve_distinguishes_gapps_from_vanilla() {
        let (_guard, dir) = EnvGuard::new();
        write_manifest(&dir, "vanilla", "13", "VANILLA", "2026-01-01T00:00:00Z");
        write_manifest(&dir, "gapps", "13", "GAPPS", "2026-01-01T00:00:00Z");

        let resolved = resolve(&profile(AndroidVersion::Android13, true)).unwrap();
        assert_eq!(resolved, dir.join("gapps.qcow2"));
    }

    #[test]
    fn manifest_without_matching_qcow2_is_skipped() {
        let (_guard, dir) = EnvGuard::new();
        fs::write(
            dir.join("orphan.manifest.json"),
            r#"{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let err = resolve(&profile(AndroidVersion::Android13, false)).unwrap_err();
        assert!(matches!(err, BaseImageError::NotFound { .. }));
    }

    #[test]
    fn malformed_manifest_is_skipped_not_fatal() {
        let (_guard, dir) = EnvGuard::new();
        fs::write(dir.join("broken.qcow2"), b"x").unwrap();
        fs::write(dir.join("broken.manifest.json"), b"{ not json").unwrap();
        write_manifest(&dir, "good", "13", "VANILLA", "2026-01-01T00:00:00Z");

        let resolved = resolve(&profile(AndroidVersion::Android13, false)).unwrap();
        assert_eq!(resolved, dir.join("good.qcow2"));
    }

    #[test]
    fn resolve_finds_image_in_version_variant_subdirectory() {
        let (_guard, dir) = EnvGuard::new();
        let subdir = dir.join("android13-vanilla");
        fs::create_dir_all(&subdir).unwrap();
        write_manifest(&subdir, "img", "13", "VANILLA", "2026-06-01T00:00:00Z");

        let resolved = resolve(&profile(AndroidVersion::Android13, false)).unwrap();
        assert_eq!(resolved, subdir.join("img.qcow2"));
    }

    #[test]
    fn resolve_prefers_freshest_across_root_and_subdirectory() {
        let (_guard, dir) = EnvGuard::new();
        let subdir = dir.join("android13-vanilla");
        fs::create_dir_all(&subdir).unwrap();
        write_manifest(&dir, "flat-old", "13", "VANILLA", "2026-01-01T00:00:00Z");
        write_manifest(
            &subdir,
            "subdir-new",
            "13",
            "VANILLA",
            "2026-06-01T00:00:00Z",
        );

        let resolved = resolve(&profile(AndroidVersion::Android13, false)).unwrap();
        assert_eq!(resolved, subdir.join("subdir-new.qcow2"));
    }

    #[test]
    fn list_all_includes_flat_and_subdirectory_images() {
        let (_guard, dir) = EnvGuard::new();
        let subdir = dir.join("android11-gapps");
        fs::create_dir_all(&subdir).unwrap();
        write_manifest(&dir, "flat", "13", "VANILLA", "2026-01-01T00:00:00Z");
        write_manifest(&subdir, "nested", "11", "GAPPS", "2026-02-01T00:00:00Z");

        let mut paths: Vec<_> = list_all()
            .unwrap()
            .into_iter()
            .map(|i| i.qcow2_path)
            .collect();
        paths.sort();
        let mut expected = vec![dir.join("flat.qcow2"), subdir.join("nested.qcow2")];
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[test]
    fn variant_label_matches_docker_build_arg_values() {
        assert_eq!(
            profile(AndroidVersion::Android13, true).variant_label(),
            "GAPPS"
        );
        assert_eq!(
            profile(AndroidVersion::Android13, false).variant_label(),
            "VANILLA"
        );
    }
}
