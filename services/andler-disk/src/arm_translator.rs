use std::collections::HashMap;
use std::path::{Path, PathBuf};

use andler_core::android_profile::ArmTranslator;

use crate::error::DiskError;
use crate::nbd;
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

pub async fn switch_translator(
    overlay_path: &Path,
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

    let nbd_guard = nbd::connect_nbd(overlay_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    // andlerd runs unprivileged: even though the partition is mounted rw, the
    // guest's root-owned directories reject raw std::fs writes with EPERM. All
    // mutations go through `sudo -n` (same pattern as guest_tools.rs and
    // boot_mode.rs); reads stay unprivileged.
    let mount = mount_guard.path();
    let waydroid_dir = match detect_waydroid_system_dir(mount)? {
        Some(dir) => dir,
        None => {
            // A freshly created Android instance may have never booted, so
            // `waydroid init` (which creates /var/lib/waydroid/overlay on first
            // boot) has not run yet. The overlay upper dir is just a directory
            // tree bind-mounted over /system by the waydroid container —
            // creating it early is safe and lets installs work pre-first-boot.
            let dir = mount.join("var/lib/waydroid/overlay");
            helper_mkdir_p(mount, &dir.join("system"))?;
            dir
        }
    };
    let system_dir = waydroid_dir.join("system");

    let current = detect_current_translator(&system_dir)?;
    if current == Some(translator) {
        tracing::info!(translator = ?translator, "translator already installed, skipping");
        return Ok(());
    }

    // Stage the new translator's files in a temp dir first and verify the copy
    // fully succeeds *before* touching the currently-installed (working) translator.
    // Previously this removed the old translator's files first and only then copied
    // the new ones in — if that copy failed partway through (disk full, permission
    // error, missing source file), the guest was left with neither translator fully
    // installed and no way to recover short of manual intervention.
    let staging_dir = system_dir.join(".andler-translator-staging");
    if staging_dir.exists() {
        helper_rm_rf(mount, &staging_dir)?;
    }
    helper_mkdir_p(mount, &staging_dir)?;

    let rel_paths: Vec<PathBuf> = match &translator_files {
        Some(files) => resolve_entry_paths(files, info.files),
        None => Vec::new(),
    };
    for rel in &rel_paths {
        // None has no payload, so rel_paths is empty — the source never
        // resolves to a real directory in that case.
        let src = match &translator_files {
            Some(files) => files.join(rel),
            None => continue,
        };
        let staged = staging_dir.join(rel);
        if src.exists() {
            if let Some(parent) = staged.parent() {
                helper_mkdir_p(mount, parent)?;
            }
            helper_cp_a(mount, &src, &staged)?;
            if rel.components().any(|c| c.as_os_str() == "bin") {
                helper_chmod(mount, 0o755, &staged)?;
            }
        } else {
            tracing::warn!(
                translator = ?translator,
                file = %rel.display(),
                "translator file missing from source, skipping"
            );
        }
    }

    // Staging succeeded in full — now it's safe to remove the old translator.
    if let Some(old) = current {
        let old_info = resolve(old);
        for rel in resolve_entry_paths(&system_dir, old_info.files) {
            helper_rm_rf(mount, &system_dir.join(rel))?;
        }
        helper_rm_rf(
            mount,
            &system_dir
                .join("etc/init")
                .join(format!("{}.rc", dir_name(old))),
        )?;
    }

    // Move the already-verified staged files into place. A rename on the same
    // filesystem (which this always is — both paths are under the same NBD-mounted
    // partition) is far more reliable than the copy loop it replaces here.
    for rel in &rel_paths {
        let staged = staging_dir.join(rel);
        let dst = system_dir.join(rel);
        if staged.exists() {
            if let Some(parent) = dst.parent() {
                helper_mkdir_p(mount, parent)?;
            }
            if dst.exists() {
                helper_rm_rf(mount, &dst)?;
            }
            helper_mv(mount, &staged, &dst)?;
        }
    }
    helper_rm_rf(mount, &staging_dir)?;

    let build_prop_path = system_dir.join("build.prop");
    // The upper build.prop shadows the base image's /system/build.prop
    // wholesale, so a partial upper must start from the base's props.
    let mut props = base_build_prop(mount)?;
    for key in MANAGED_PROP_KEYS {
        props.remove(*key);
    }
    for (key, value) in info.props {
        props.insert(key.to_string(), value.to_string());
    }
    helper_guest_write(
        mount,
        &build_prop_path,
        build_prop_content(&props).as_bytes(),
    )?;

    if let Some(rc_content) = info.init_rc {
        let rc_path = system_dir
            .join("etc/init")
            .join(format!("{}.rc", dir_name(translator)));
        if let Some(parent) = rc_path.parent() {
            helper_mkdir_p(mount, parent)?;
        }
        helper_guest_write(mount, &rc_path, rc_content.as_bytes())?;
    }

    Ok(())
}

fn detect_current_translator(system_dir: &Path) -> Result<Option<ArmTranslator>, DiskError> {
    for (translator, detect_path) in &[
        (ArmTranslator::Libndk, crate::translator::ndk::DETECT_FILE),
        (
            ArmTranslator::Libhoudini,
            crate::translator::houdini::DETECT_FILE,
        ),
    ] {
        if system_dir.join(detect_path).exists() {
            return Ok(Some(*translator));
        }
    }
    Ok(None)
}

fn detect_waydroid_system_dir(mount_point: &Path) -> Result<Option<PathBuf>, DiskError> {
    let waydroid_overlay = mount_point.join("var/lib/waydroid/overlay");
    if waydroid_overlay.exists() {
        return Ok(Some(waydroid_overlay));
    }

    let alt_overlay = mount_point.join("overlay");
    if alt_overlay.join("system").exists() {
        return Ok(Some(alt_overlay));
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

fn read_build_prop(path: &Path) -> Result<HashMap<String, String>, DiskError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| DiskError::FileSystem(format!("failed to read build.prop: {e}")))?;
    Ok(parse_build_prop(&content))
}

/// The guest's pristine build.prop *below* the waydroid overlay. The upper
/// overlay build.prop shadows it wholesale, so the upper must always start
/// from the base file — the existing upper is deliberately not a source: it
/// is derived data, and reinstalling regenerates it (an upper written by a
/// buggy build contains only translator props and would hide the base's
/// `ro.*` props, breaking Android boot). Two guest layouts exist: a plain
/// `<root>/system/build.prop`, and the waydroid mainline layout where the
/// Android system is a loop-mounted `etc/waydroid-extra/images/system.img`
/// (ext4) — the file is extracted with `debugfs` (e2fsprogs, essential on
/// Debian/Arch, needs no root to read the image).
fn base_build_prop(mount_point: &Path) -> Result<HashMap<String, String>, DiskError> {
    let plain = mount_point.join("system/build.prop");
    if plain.is_file() {
        return read_build_prop(&plain);
    }
    let image = mount_point.join("etc/waydroid-extra/images/system.img");
    if image.is_file() {
        let output = std::process::Command::new("debugfs")
            .args(["-R", "cat /system/build.prop"])
            .arg(&image)
            .output()
            .map_err(|e| DiskError::FileSystem(format!("failed to run debugfs: {e}")))?;
        if output.status.success() {
            return Ok(parse_build_prop(&String::from_utf8_lossy(&output.stdout)));
        }
        return Err(DiskError::FileSystem(format!(
            "cannot extract /system/build.prop from {}: debugfs exited with {} ({})",
            image.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Err(DiskError::FileSystem(format!(
        "no base build.prop found: neither {} nor the waydroid system image {} \
         exists; refusing to write an upper build.prop that would shadow the \
         base's props wholesale",
        plain.display(),
        image.display()
    )))
}

fn build_prop_content(props: &HashMap<String, String>) -> String {
    let mut lines: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

fn helper_file_output(
    mount: &Path,
    op: &str,
    paths: &[&str],
) -> Result<std::process::Output, DiskError> {
    nbd::helper_command("file")
        .arg(mount)
        .arg(op)
        .args(paths)
        .output()
        .map_err(|e| DiskError::FileSystem(format!("failed to run andler-helper file {op}: {e}")))
}

fn helper_ok(mount: &Path, op: &str, paths: &[&str], what: &str) -> Result<(), DiskError> {
    let output = helper_file_output(mount, op, paths)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::FileSystem(format!(
            "{what} failed ({}): {}",
            output.status,
            nbd::describe_helper_failure(stderr.trim())
        )));
    }
    Ok(())
}

fn helper_mkdir_p(mount: &Path, path: &Path) -> Result<(), DiskError> {
    helper_ok(
        mount,
        "mkdir-p",
        &[&path.to_string_lossy()],
        &format!("mkdir {}", path.display()),
    )
}

fn helper_cp_a(mount: &Path, src: &Path, dst: &Path) -> Result<(), DiskError> {
    helper_ok(
        mount,
        "cp-a",
        &[&src.to_string_lossy(), &dst.to_string_lossy()],
        &format!("cp {} -> {}", src.display(), dst.display()),
    )
}

fn helper_mv(mount: &Path, src: &Path, dst: &Path) -> Result<(), DiskError> {
    helper_ok(
        mount,
        "mv",
        &[&src.to_string_lossy(), &dst.to_string_lossy()],
        &format!("mv {} -> {}", src.display(), dst.display()),
    )
}

fn helper_rm_rf(mount: &Path, path: &Path) -> Result<(), DiskError> {
    helper_ok(
        mount,
        "rm-rf",
        &[&path.to_string_lossy()],
        &format!("rm {}", path.display()),
    )
}

fn helper_chmod(mount: &Path, mode: u32, path: &Path) -> Result<(), DiskError> {
    let mode_str = format!("{mode:o}");
    helper_ok(
        mount,
        "chmod",
        &[&mode_str, &path.to_string_lossy()],
        &format!("chmod {} {}", mode, path.display()),
    )
}

fn helper_guest_write(mount: &Path, path: &Path, content: &[u8]) -> Result<(), DiskError> {
    // Content travels over the helper's stdin; the helper chroots and replaces
    // the target file itself (removing a possibly-dangling symlink first), so
    // the guest write no longer needs a host-side temp file roundtrip.
    use std::io::Write;
    let mut child = nbd::helper_command("guest-write")
        .arg(mount)
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            DiskError::FileSystem(format!("failed to run andler-helper guest-write: {e}"))
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content)
            .map_err(|e| DiskError::FileSystem(format!("failed to feed andler-helper: {e}")))?;
    }
    let output = child.wait_with_output().map_err(|e| {
        DiskError::FileSystem(format!("failed to run andler-helper guest-write: {e}"))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::FileSystem(format!(
            "write {} failed ({}): {}",
            path.display(),
            output.status,
            nbd::describe_helper_failure(stderr.trim())
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_current_translator_returns_none_on_empty_dir() {
        let dir = std::env::temp_dir().join("andler_test_empty_dir");
        std::fs::create_dir_all(&dir).unwrap();
        let result = detect_current_translator(&dir).unwrap();
        assert!(result.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detect_waydroid_system_dir_returns_none_on_never_booted_image() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_waydroid_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(detect_waydroid_system_dir(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_waydroid_system_dir_prefers_standard_overlay_path() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_waydroid2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("var/lib/waydroid/overlay/system")).unwrap();
        std::fs::create_dir_all(dir.join("overlay/system")).unwrap();
        let result = detect_waydroid_system_dir(&dir).unwrap();
        assert_eq!(
            result,
            Some(dir.join("var/lib/waydroid/overlay")),
            "the standard waydroid overlay path must win over the legacy fallback"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn base_build_prop_reads_plain_layout_and_errors_without_source() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_props_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("system")).unwrap();

        let err = base_build_prop(&dir)
            .err()
            .expect("no base source must be a hard error, not a silent empty set");
        let text = err.to_string();
        assert!(
            text.contains("no base build.prop found"),
            "error must name the missing sources: {text}"
        );

        std::fs::write(
            dir.join("system/build.prop"),
            "# comment\nro.product.model=Base\nro.build.version.sdk=33\n",
        )
        .unwrap();
        let props = base_build_prop(&dir).unwrap();
        assert_eq!(
            props.get("ro.product.model").map(|s| s.as_str()),
            Some("Base")
        );
        assert_eq!(
            props.get("ro.build.version.sdk").map(|s| s.as_str()),
            Some("33"),
            "comment lines must be skipped, real keys parsed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn base_build_prop_prefers_plain_layout_over_system_image() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_img_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("system")).unwrap();
        std::fs::create_dir_all(dir.join("etc/waydroid-extra/images")).unwrap();
        std::fs::write(dir.join("system/build.prop"), "ro.product.model=Plain\n").unwrap();
        // The image file is not a real ext4 here — it must not even be
        // touched, the plain layout wins.
        std::fs::write(
            dir.join("etc/waydroid-extra/images/system.img"),
            b"not an image",
        )
        .unwrap();

        let props = base_build_prop(&dir).unwrap();
        assert_eq!(
            props.get("ro.product.model").map(|s| s.as_str()),
            Some("Plain")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_build_prop_skips_blank_and_comment_lines() {
        let props = parse_build_prop("# header\n\nro.a=1\nro.b= two\nro.a=2\n");
        assert_eq!(props.get("ro.a").map(|s| s.as_str()), Some("2"));
        assert_eq!(props.get("ro.b").map(|s| s.as_str()), Some("two"));
        assert_eq!(props.len(), 2);
    }
}
