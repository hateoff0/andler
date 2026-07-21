

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

    let waydroid_dir = detect_waydroid_system_dir(mount_guard.path())?;
    let system_dir = waydroid_dir.join("system");

    let current = detect_current_translator(&system_dir)?;
    if current == Some(translator) {
        tracing::info!(translator = ?translator, "translator already installed, skipping");
        return Ok(());
    }

    if let Some(old) = current {
        let old_info = resolve(old);
        for file in old_info.files {
            let path = system_dir.join(file);
            if path.exists() {
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                        .map_err(|e| DiskError::FileSystem(format!("failed to remove old translator dir {path:?}: {e}")))?;
                } else {
                    std::fs::remove_file(&path)
                        .map_err(|e| DiskError::FileSystem(format!("failed to remove old translator file {path:?}: {e}")))?;
                }
            }
        }
        let old_init_rc = system_dir.join("etc/init").join(format!("{}.rc", dir_name(old)));
        if old_init_rc.exists() {
            std::fs::remove_file(&old_init_rc)
                .map_err(|e| DiskError::FileSystem(format!("failed to remove old init.rc {old_init_rc:?}: {e}")))?;
        }
    }

    for file in info.files {
        let src = translator_files.join(file);
        let dst = system_dir.join(file);
        if src.exists() {
            if src.is_dir() {
                copy_dir_recursive(&src, &dst)?;
            } else {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        DiskError::FileSystem(format!("failed to create parent dir for {dst:?}: {e}"))
                    })?;
                }
                std::fs::copy(&src, &dst).map_err(|e| {
                    DiskError::FileSystem(format!("failed to copy translator file {src:?} -> {dst:?}: {e}"))
                })?;
            }
        }
    }

    let build_prop_path = system_dir.join("build.prop");
    let mut props = read_build_prop(&build_prop_path)?;
    for (key, value) in info.props {
        props.insert(key.to_string(), value.to_string());
    }
    write_build_prop(&build_prop_path, &props)?;

    if let Some(rc_content) = info.init_rc {
        let init_rc_path = system_dir
            .join("etc/init")
            .join(format!("{}.rc", dir_name(translator)));
        std::fs::write(&init_rc_path, rc_content)
            .map_err(|e| DiskError::FileSystem(format!("failed to write init.rc for {translator:?}: {e}")))?;
    }

    Ok(())
}


fn detect_current_translator(system_dir: &Path) -> Result<Option<ArmTranslator>, DiskError> {
    for (translator, detect_path) in &[
        (ArmTranslator::Libndk, crate::translator::ndk::DETECT_FILE),
        (ArmTranslator::Libhoudini, crate::translator::houdini::DETECT_FILE),
    ] {
        if system_dir.join(detect_path).exists() {
            return Ok(Some(*translator));
        }
    }
    Ok(None)
}


fn detect_waydroid_system_dir(mount_point: &Path) -> Result<PathBuf, DiskError> {
    let waydroid_overlay = mount_point.join("var/lib/waydroid/overlay");
    if waydroid_overlay.exists() {
        return Ok(waydroid_overlay);
    }

    let alt_overlay = mount_point.join("overlay");
    if alt_overlay.join("system").exists() {
        return Ok(alt_overlay);
    }

    Err(DiskError::FileSystem(
        "Waydroid overlay directory not found in guest filesystem".to_string(),
    ))
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


fn write_build_prop(path: &Path, props: &HashMap<String, String>) -> Result<(), DiskError> {
    let mut lines: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    let content = lines.join("\n") + "\n";
    std::fs::write(path, content)
        .map_err(|e| DiskError::FileSystem(format!("failed to write build.prop: {e}")))?;
    Ok(())
}


fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), DiskError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| DiskError::FileSystem(format!("failed to create dir {dst:?}: {e}")))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| DiskError::FileSystem(format!("failed to read dir {src:?}: {e}")))?
    {
        let entry =
            entry.map_err(|e| DiskError::FileSystem(format!("failed to read entry: {e}")))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                DiskError::FileSystem(format!("failed to copy {src_path:?}: {e}"))
            })?;
            if dst_path.components().any(|c| c.as_os_str() == "bin") {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&dst_path)
                    .map_err(|e| DiskError::FileSystem(format!("failed to read permissions for {dst_path:?}: {e}")))?
                    .permissions();
                perms.set_mode(perms.mode() | 0o755);
                std::fs::set_permissions(&dst_path, perms)
                    .map_err(|e| DiskError::FileSystem(format!("failed to set permissions for {dst_path:?}: {e}")))?;
            }
        }
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
}
