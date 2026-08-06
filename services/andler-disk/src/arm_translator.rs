use std::collections::HashMap;
use std::path::{Path, PathBuf};

use andler_core::android_profile::ArmTranslator;

use crate::error::DiskError;
use crate::nbd;
use crate::translator::{dir_name, resolve};
use crate::translator_download;

pub async fn switch_translator(
    overlay_path: &Path,
    translator: ArmTranslator,
    translator_dir: Option<PathBuf>,
    android_version: &str,
) -> Result<(), DiskError> {
    let info = resolve(translator);

    let translator_files = match translator_dir {
        Some(dir) => dir,
        None => translator_download::ensure_translator(translator, android_version).await?,
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
            sudo_mkdir_p(&dir.join("system"))?;
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
        sudo_rm_rf(&staging_dir)?;
    }
    sudo_mkdir_p(&staging_dir)?;

    for file in info.files {
        let src = translator_files.join(file);
        let staged = staging_dir.join(file);
        if src.exists() {
            if let Some(parent) = staged.parent() {
                sudo_mkdir_p(parent)?;
            }
            sudo_cp_a(&src, &staged)?;
            if file.split('/').any(|c| c == "bin") {
                sudo_chmod(0o755, &staged)?;
            }
        }
    }

    // Staging succeeded in full — now it's safe to remove the old translator.
    if let Some(old) = current {
        let old_info = resolve(old);
        for file in old_info.files {
            sudo_rm_rf(&system_dir.join(file))?;
        }
        sudo_rm_rf(
            &system_dir
                .join("etc/init")
                .join(format!("{}.rc", dir_name(old))),
        )?;
    }

    // Move the already-verified staged files into place. A rename on the same
    // filesystem (which this always is — both paths are under the same NBD-mounted
    // partition) is far more reliable than the copy loop it replaces here.
    for file in info.files {
        let staged = staging_dir.join(file);
        let dst = system_dir.join(file);
        if staged.exists() {
            if let Some(parent) = dst.parent() {
                sudo_mkdir_p(parent)?;
            }
            if dst.exists() {
                sudo_rm_rf(&dst)?;
            }
            sudo_mv(&staged, &dst)?;
        }
    }
    sudo_rm_rf(&staging_dir)?;

    let build_prop_path = system_dir.join("build.prop");
    let mut props = read_build_prop(&build_prop_path)?;
    for (key, value) in info.props {
        props.insert(key.to_string(), value.to_string());
    }
    sudo_write(&build_prop_path, build_prop_content(&props).as_bytes())?;

    if let Some(rc_content) = info.init_rc {
        sudo_write(
            &system_dir
                .join("etc/init")
                .join(format!("{}.rc", dir_name(translator))),
            rc_content.as_bytes(),
        )?;
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

fn read_build_prop(path: &Path) -> Result<HashMap<String, String>, DiskError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| DiskError::FileSystem(format!("failed to read build.prop: {e}")))?;
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
    Ok(props)
}

fn build_prop_content(props: &HashMap<String, String>) -> String {
    let mut lines: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

fn sudo_output(args: &[&str]) -> Result<std::process::Output, DiskError> {
    nbd::privileged_command(args[0])
        .args(&args[1..])
        .output()
        .map_err(|e| DiskError::FileSystem(format!("failed to run sudo {}: {e}", args[0])))
}

fn sudo_ok(args: &[&str], what: &str) -> Result<(), DiskError> {
    let output = sudo_output(args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::FileSystem(format!(
            "{what} failed ({}): {}",
            output.status,
            nbd::describe_sudo_failure(args[0], stderr.trim())
        )));
    }
    Ok(())
}

fn sudo_mkdir_p(path: &Path) -> Result<(), DiskError> {
    sudo_ok(
        &["mkdir", "-p", &path.to_string_lossy()],
        &format!("mkdir {}", path.display()),
    )
}

fn sudo_cp_a(src: &Path, dst: &Path) -> Result<(), DiskError> {
    sudo_ok(
        &["cp", "-a", &src.to_string_lossy(), &dst.to_string_lossy()],
        &format!("cp {} -> {}", src.display(), dst.display()),
    )
}

fn sudo_mv(src: &Path, dst: &Path) -> Result<(), DiskError> {
    sudo_ok(
        &["mv", &src.to_string_lossy(), &dst.to_string_lossy()],
        &format!("mv {} -> {}", src.display(), dst.display()),
    )
}

fn sudo_rm_rf(path: &Path) -> Result<(), DiskError> {
    sudo_ok(
        &["rm", "-rf", &path.to_string_lossy()],
        &format!("rm {}", path.display()),
    )
}

fn sudo_chmod(mode: u32, path: &Path) -> Result<(), DiskError> {
    sudo_ok(
        &["chmod", &format!("{mode:o}"), &path.to_string_lossy()],
        &format!("chmod {} {}", mode, path.display()),
    )
}

fn sudo_write(path: &Path, content: &[u8]) -> Result<(), DiskError> {
    // Write through a host-side temp file so sudo never has to read our content
    // from stdin, then move it into the guest filesystem with cp.
    let temp = std::env::temp_dir().join(format!(
        "andler-translator-write-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(&temp, content)
        .map_err(|e| DiskError::FileSystem(format!("failed to write temp file: {e}")))?;
    let result = sudo_cp_a(&temp, path);
    let _ = std::fs::remove_file(&temp);
    result
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
}
