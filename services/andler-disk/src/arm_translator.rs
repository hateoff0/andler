use std::collections::HashMap;
use std::path::{Path, PathBuf};

use andler_core::android_profile::ArmTranslator;
use andler_core::{GuestMutator, MutatorOp};

use crate::error::DiskError;
use crate::translator::{dir_name, resolve, MANAGED_PROP_KEYS};
use crate::translator_download;

fn resolve_entry_paths(root: &Path, files: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for file in files {
        if let Some(prefix) = file.strip_suffix('*') {
            // `dir/name*` matches entries under `dir` starting with `name`;
            // `dir/*` matches every entry under `dir` (a '*' glued to a slash
            // means "everything in that directory", not "a name starting with
            // the directory's own name").
            let (parent, name_prefix) = if prefix.ends_with('/') {
                (PathBuf::from(prefix.trim_end_matches('/')), String::new())
            } else {
                let p = Path::new(prefix);
                (
                    p.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            let entries = match std::fs::read_dir(root.join(&parent)) {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(
                        root = %root.display(),
                        pattern = %file,
                        error = %e,
                        "translator entry pattern does not expand; skipping"
                    );
                    continue;
                }
            };
            let mut matched: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&name_prefix))
                .map(|e| parent.join(e.file_name()))
                .collect();
            out.append(&mut matched);
        } else {
            out.push(PathBuf::from(file));
        }
    }
    out.sort();
    out.dedup();
    out
}

pub async fn switch_translator_with(
    mutator: &dyn GuestMutator,
    translator: ArmTranslator,
    translator_dir: Option<PathBuf>,
    android_version: &str,
) -> Result<(), DiskError> {
    let info = resolve(translator);

    // None has no payload and no download link — the removal path only. The
    // download must not be attempted for it (ensure_translator would fail
    // with "no download available for None").
    let translator_files: Option<PathBuf> = match translator_dir {
        Some(dir) => Some(dir),
        None if translator == ArmTranslator::None => None,
        None => Some(translator_download::ensure_translator(translator, android_version).await?),
    };

    let waydroid_dir = match detect_waydroid_system_dir_with(mutator).await? {
        Some(dir) => dir,
        None => {
            // A freshly created Android instance may have never booted, so
            // `waydroid init` (which creates /var/lib/waydroid/overlay on first
            // boot) has not run yet. The overlay upper dir is just a directory
            // tree bind-mounted over /system by the waydroid container —
            // creating it early is safe and lets installs work pre-first-boot.
            let dir = "/var/lib/waydroid/overlay".to_string();
            mutator
                .apply(&[MutatorOp::MkdirP {
                    path: format!("{dir}/system"),
                }])
                .await
                .map_err(|e| DiskError::FileSystem(format!("failed to create overlay dir: {e}")))?;
            dir
        }
    };
    let system_dir = format!("{waydroid_dir}/system");

    let current = detect_current_translator_with(mutator, &system_dir).await?;
    if current == Some(translator) {
        tracing::info!(translator = ?translator, "translator already installed, skipping");
        return Ok(());
    }

    // Stage the new translator's files first and verify every upload fully
    // succeeds *before* touching the currently-installed (working) translator.
    // Previously this removed the old translator's files first and only then
    // copied the new ones in — a partial failure left the guest with neither
    // translator fully installed and no way to recover short of manual
    // intervention. One appliance session per batch (phase 4 requirement).
    let staging = format!("{system_dir}/.andler-translator-staging");
    let rel_paths: Vec<PathBuf> = match &translator_files {
        Some(files) => resolve_entry_paths(files, info.files),
        None => Vec::new(),
    };

    let mut staging_ops = vec![
        MutatorOp::RmRf {
            path: staging.clone(),
        },
        MutatorOp::MkdirP {
            path: staging.clone(),
        },
    ];
    for rel in &rel_paths {
        // None has no payload, so rel_paths is empty — the source never
        // resolves to a real directory in that case.
        let src = match &translator_files {
            Some(files) => files.join(rel),
            None => continue,
        };
        if !src.exists() {
            tracing::warn!(
                translator = ?translator,
                file = %rel.display(),
                "translator file missing from source, skipping"
            );
            continue;
        }
        let staged = Path::new(&staging).join(rel);
        if let Some(parent) = staged.parent() {
            staging_ops.push(MutatorOp::MkdirP {
                path: parent.to_string_lossy().into_owned(),
            });
        }
        staging_ops.push(MutatorOp::UploadFile {
            path: staged.to_string_lossy().into_owned(),
            host_path: src,
        });
        if rel.components().any(|c| c.as_os_str() == "bin") {
            staging_ops.push(MutatorOp::Chmod {
                path: staged.to_string_lossy().into_owned(),
                mode: 0o755,
            });
        }
    }
    mutator
        .apply(&staging_ops)
        .await
        .map_err(|e| DiskError::FileSystem(format!("failed to stage translator: {e}")))?;

    // Staging succeeded in full — now it's safe to remove the old translator.
    let mut cleanup_ops = Vec::new();
    if let Some(old) = current {
        let old_info = resolve(old);
        for rel in resolve_entry_paths(Path::new(&system_dir), old_info.files) {
            cleanup_ops.push(MutatorOp::RmRf {
                path: format!("{system_dir}/{}", rel.display()),
            });
        }
        cleanup_ops.push(MutatorOp::RmRf {
            path: format!("{system_dir}/etc/init/{}.rc", dir_name(old)),
        });
        mutator
            .apply(&cleanup_ops)
            .await
            .map_err(|e| DiskError::FileSystem(format!("failed to remove old translator: {e}")))?;
    }

    // Move the already-verified staged files into place (same filesystem
    // rename — more reliable than a second copy loop).
    let mut move_ops = Vec::new();
    for rel in &rel_paths {
        let staged = Path::new(&staging).join(rel);
        let dst = Path::new(&system_dir).join(rel);
        let staged_guest = staged.to_string_lossy().into_owned();
        let dst_guest = dst.to_string_lossy().into_owned();
        if !mutator
            .exists(&staged_guest)
            .await
            .map_err(|e| DiskError::FileSystem(format!("failed to stat staged file: {e}")))?
        {
            continue;
        }
        if let Some(parent) = dst.parent() {
            move_ops.push(MutatorOp::MkdirP {
                path: parent.to_string_lossy().into_owned(),
            });
        }
        if mutator
            .exists(&dst_guest)
            .await
            .map_err(|e| DiskError::FileSystem(format!("failed to stat destination: {e}")))?
        {
            move_ops.push(MutatorOp::RmRf {
                path: dst_guest.clone(),
            });
        }
        move_ops.push(MutatorOp::Mv {
            src: staged_guest,
            dst: dst_guest,
        });
    }
    move_ops.push(MutatorOp::RmRf {
        path: staging.clone(),
    });
    mutator
        .apply(&move_ops)
        .await
        .map_err(|e| DiskError::FileSystem(format!("failed to install translator: {e}")))?;

    let build_prop_path = format!("{system_dir}/build.prop");
    // The upper build.prop shadows the base image's /system/build.prop
    // wholesale, so a partial upper must start from the base's props.
    let mut props = base_build_prop_with(mutator).await?;
    for key in MANAGED_PROP_KEYS {
        props.remove(*key);
    }
    for (key, value) in info.props {
        props.insert(key.to_string(), value.to_string());
    }
    let mut writes = vec![MutatorOp::WriteFile {
        path: build_prop_path.clone(),
        content: build_prop_content(&props).into_bytes(),
    }];
    if let Some(rc_content) = info.init_rc {
        let rc_path = format!("{system_dir}/etc/init/{}.rc", dir_name(translator));
        writes.push(MutatorOp::WriteFile {
            path: rc_path,
            content: rc_content.as_bytes().to_vec(),
        });
    }
    mutator
        .apply(&writes)
        .await
        .map_err(|e| DiskError::FileSystem(format!("failed to write translator config: {e}")))?;

    Ok(())
}

async fn detect_current_translator_with(
    mutator: &dyn GuestMutator,
    system_dir: &str,
) -> Result<Option<ArmTranslator>, DiskError> {
    for (translator, detect_path) in &[
        (ArmTranslator::Libndk, crate::translator::ndk::DETECT_FILE),
        (
            ArmTranslator::Libhoudini,
            crate::translator::houdini::DETECT_FILE,
        ),
    ] {
        let path = format!("{system_dir}/{detect_path}");
        if mutator
            .exists(&path)
            .await
            .map_err(|e| DiskError::FileSystem(format!("failed to inspect {path}: {e}")))?
        {
            return Ok(Some(*translator));
        }
    }
    Ok(None)
}

async fn detect_waydroid_system_dir_with(
    mutator: &dyn GuestMutator,
) -> Result<Option<String>, DiskError> {
    if mutator
        .exists("/var/lib/waydroid/overlay")
        .await
        .map_err(|e| DiskError::FileSystem(format!("failed to inspect guest: {e}")))?
    {
        return Ok(Some("/var/lib/waydroid/overlay".to_string()));
    }
    if mutator
        .exists("/overlay/system")
        .await
        .map_err(|e| DiskError::FileSystem(format!("failed to inspect guest: {e}")))?
    {
        return Ok(Some("/overlay".to_string()));
    }
    Ok(None)
}

fn parse_build_prop(content: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            props.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    props
}

/// The guest's pristine build.prop *below* the waydroid overlay. The upper
/// overlay build.prop shadows it wholesale, so the upper must always start
/// from the base file — the existing upper is deliberately not a source: it
/// is derived data, and reinstalling regenerates it (an upper written by a
/// buggy build contains only translator props and would hide the base's
/// `ro.*` props, breaking Android boot). Two guest layouts exist: a plain
/// `/system/build.prop`, and the waydroid mainline layout where the Android
/// system is a loop-mounted `etc/waydroid-extra/images/system.img` (ext4).
/// The image is read out of the guest and extracted with `debugfs`
/// (e2fsprogs, needs no root); images are small (a few MB).
async fn base_build_prop_with(
    mutator: &dyn GuestMutator,
) -> Result<HashMap<String, String>, DiskError> {
    match mutator.read_file("/system/build.prop").await {
        Ok(content) => return Ok(parse_build_prop(&String::from_utf8_lossy(&content))),
        Err(err) => tracing::debug!("no plain /system/build.prop: {err}"),
    }

    match mutator
        .read_file("/etc/waydroid-extra/images/system.img")
        .await
    {
        Ok(image) => {
            let tmp = std::env::temp_dir().join(format!(
                "andler-system-img-{}-{}.img",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::write(&tmp, &image)
                .map_err(|e| DiskError::FileSystem(format!("failed to stage system image: {e}")))?;
            let output = std::process::Command::new("debugfs")
                .args(["-R", "cat /system/build.prop"])
                .arg(&tmp)
                .output()
                .map_err(|e| DiskError::FileSystem(format!("failed to run debugfs: {e}")))?;
            let _ = std::fs::remove_file(&tmp);
            if output.status.success() {
                return Ok(parse_build_prop(&String::from_utf8_lossy(&output.stdout)));
            }
            return Err(DiskError::FileSystem(format!(
                "cannot extract /system/build.prop from the waydroid system image: \
                 debugfs exited with {} ({})",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Err(err) => tracing::debug!("no waydroid system image either: {err}"),
    }

    Err(DiskError::FileSystem(
        "no base build.prop found: neither /system/build.prop nor the waydroid \
         system image /etc/waydroid-extra/images/system.img exists; refusing to \
         write an upper build.prop that would shadow the base's props wholesale"
            .to_string(),
    ))
}

fn build_prop_content(props: &HashMap<String, String>) -> String {
    let mut lines: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GuestMutator over a local directory — lets the guest-path logic be
    /// unit-tested without an appliance or a guest agent.
    struct TestMutator {
        root: std::path::PathBuf,
    }

    impl TestMutator {
        fn new() -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "andler-test-mutator-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            TestMutator { root }
        }

        fn guest(&self, path: &str) -> std::path::PathBuf {
            self.root.join(path.trim_start_matches('/'))
        }
    }

    #[async_trait::async_trait]
    impl GuestMutator for TestMutator {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn apply(&self, ops: &[MutatorOp]) -> Result<(), andler_core::MutatorError> {
            for op in ops {
                match op {
                    MutatorOp::WriteFile { path, content } => {
                        if let Some(parent) = self.guest(path).parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                        }
                        std::fs::write(self.guest(path), content)
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::UploadFile { path, host_path } => {
                        if let Some(parent) = self.guest(path).parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                        }
                        std::fs::copy(host_path, self.guest(path))
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::MkdirP { path } => {
                        std::fs::create_dir_all(self.guest(path))
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::CpA { src, dst } => {
                        std::fs::copy(self.guest(src), self.guest(dst))
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::Mv { src, dst } => {
                        if let Some(parent) = self.guest(dst).parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                        }
                        std::fs::rename(self.guest(src), self.guest(dst))
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::RmRf { path } => {
                        let p = self.guest(path);
                        if p.exists() {
                            std::fs::remove_dir_all(&p)
                                .or_else(|_| std::fs::remove_file(&p))
                                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                        }
                    }
                    MutatorOp::Chmod { path, mode } => {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            self.guest(path),
                            std::fs::Permissions::from_mode(*mode),
                        )
                        .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                    MutatorOp::Symlink { target, link } => {
                        if let Some(parent) = self.guest(link).parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                        }
                        let _ = std::fs::remove_file(self.guest(link));
                        std::os::unix::fs::symlink(target, self.guest(link))
                            .map_err(|e| andler_core::MutatorError::Io(e.to_string()))?;
                    }
                }
            }
            Ok(())
        }

        async fn read_file(&self, path: &str) -> Result<Vec<u8>, andler_core::MutatorError> {
            std::fs::read(self.guest(path))
                .map_err(|e| andler_core::MutatorError::Io(e.to_string()))
        }

        async fn exists(&self, path: &str) -> Result<bool, andler_core::MutatorError> {
            Ok(self.guest(path).exists())
        }
    }

    #[tokio::test]
    async fn detect_current_translator_returns_none_on_empty_dir() {
        let m = TestMutator::new();
        let result = detect_current_translator_with(&m, "/system").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn detect_waydroid_system_dir_returns_none_on_never_booted_image() {
        let m = TestMutator::new();
        assert_eq!(detect_waydroid_system_dir_with(&m).await.unwrap(), None);
    }

    #[tokio::test]
    async fn detect_waydroid_system_dir_prefers_standard_overlay_path() {
        let m = TestMutator::new();
        m.apply(&[MutatorOp::MkdirP {
            path: "/var/lib/waydroid/overlay/system".to_string(),
        }])
        .await
        .unwrap();
        m.apply(&[MutatorOp::MkdirP {
            path: "/overlay/system".to_string(),
        }])
        .await
        .unwrap();
        let result = detect_waydroid_system_dir_with(&m).await.unwrap();
        assert_eq!(
            result,
            Some("/var/lib/waydroid/overlay".to_string()),
            "the standard waydroid overlay path must win over the legacy fallback"
        );
    }

    #[test]
    fn build_prop_content_sorts_keys_and_appends_newline() {
        let mut props = HashMap::new();
        props.insert(
            "ro.dalvik.vm.native.bridge".to_string(),
            "libndk_translation.so".to_string(),
        );
        props.insert("ro.enable.native.bridge.exec".to_string(), "1".to_string());
        let content = build_prop_content(&props);
        assert!(content.ends_with('\n'));
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines.windows(2).all(|w| w[0] < w[1]),
            "props must be sorted"
        );
    }

    #[test]
    fn resolve_entry_paths_expands_wildcards_in_parent_dir() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_entries_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/libndk_translation.so"), b"x").unwrap();
        std::fs::write(dir.join("lib/libndk_proxy.so"), b"x").unwrap();
        std::fs::write(dir.join("lib/unrelated.so"), b"x").unwrap();

        let paths = resolve_entry_paths(
            &dir,
            &[
                "bin/houdini",
                "lib/libndk*",
                "etc/binfmt_misc",
                "lib64/libndk*",
            ],
        );
        let names: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "bin/houdini",
                "etc/binfmt_misc",
                "lib/libndk_proxy.so",
                "lib/libndk_translation.so",
            ],
            "an unmatched wildcard must expand to nothing, not a literal * path"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_entry_paths_returns_empty_for_unmatched_wildcard() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_entries2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = resolve_entry_paths(&dir, &["lib/libndk*"]);
        assert!(paths.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_entry_paths_slash_star_matches_whole_directory() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_entries3_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::write(dir.join("etc/cpuinfo.arm.txt"), b"x").unwrap();
        std::fs::write(dir.join("etc/ld.config.arm.txt"), b"x").unwrap();

        let paths = resolve_entry_paths(&dir, &["etc/*"]);
        let names: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["etc/cpuinfo.arm.txt", "etc/ld.config.arm.txt"],
            "dir/* must expand to every entry under dir, not a literal * path"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn base_build_prop_reads_plain_layout_and_errors_without_source() {
        let m = TestMutator::new();
        m.apply(&[MutatorOp::MkdirP {
            path: "/system".to_string(),
        }])
        .await
        .unwrap();

        let err = base_build_prop_with(&m)
            .await
            .err()
            .expect("no base source must be a hard error, not a silent empty set");
        let text = err.to_string();
        assert!(
            text.contains("no base build.prop found"),
            "error must name the missing sources: {text}"
        );

        m.apply(&[MutatorOp::WriteFile {
            path: "/system/build.prop".to_string(),
            content: b"# comment\nro.product.model=Base\nro.build.version.sdk=33\n".to_vec(),
        }])
        .await
        .unwrap();
        let props = base_build_prop_with(&m).await.unwrap();
        assert_eq!(
            props.get("ro.product.model").map(|s| s.as_str()),
            Some("Base")
        );
        assert_eq!(
            props.get("ro.build.version.sdk").map(|s| s.as_str()),
            Some("33"),
            "comment lines must be skipped, real keys parsed"
        );
    }

    #[tokio::test]
    async fn base_build_prop_prefers_plain_layout_over_system_image() {
        let m = TestMutator::new();
        m.apply(&[
            MutatorOp::MkdirP {
                path: "/system".to_string(),
            },
            MutatorOp::MkdirP {
                path: "/etc/waydroid-extra/images".to_string(),
            },
            MutatorOp::WriteFile {
                path: "/system/build.prop".to_string(),
                content: b"ro.product.model=Plain\n".to_vec(),
            },
            // The image file is not a real ext4 here — it must not even be
            // touched, the plain layout wins.
            MutatorOp::WriteFile {
                path: "/etc/waydroid-extra/images/system.img".to_string(),
                content: b"not an image".to_vec(),
            },
        ])
        .await
        .unwrap();

        let props = base_build_prop_with(&m).await.unwrap();
        assert_eq!(
            props.get("ro.product.model").map(|s| s.as_str()),
            Some("Plain")
        );
    }

    #[test]
    fn parse_build_prop_skips_blank_and_comment_lines() {
        let props = parse_build_prop("# header\n\nro.a=1\nro.b= two\nro.a=2\n");
        assert_eq!(props.get("ro.a").map(|s| s.as_str()), Some("2"));
        assert_eq!(props.get("ro.b").map(|s| s.as_str()), Some("two"));
        assert_eq!(props.len(), 2);
    }
}
