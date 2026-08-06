use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::error::DiskError;

pub struct DiskInfo {
    pub virtual_size: u64,

    pub actual_size: u64,

    pub format: String,

    pub backing_file: Option<String>,
}

pub async fn create(path: &Path, size_bytes: u64) -> Result<(), DiskError> {
    ensure_parent_dir_exists(path).await?;

    run_qemu_img(&[
        "create",
        "-f",
        "qcow2",
        &path.to_string_lossy(),
        &size_bytes.to_string(),
    ])
    .await
}

pub async fn create_with_backing_file(
    path: &Path,
    backing_file: &Path,
    size_bytes: u64,
) -> Result<(), DiskError> {
    if !backing_file.exists() {
        return Err(DiskError::BackingFileNotFound(backing_file.to_path_buf()));
    }

    ensure_parent_dir_exists(path).await?;

    run_qemu_img(&[
        "create",
        "-f",
        "qcow2",
        "-F",
        "qcow2",
        "-b",
        &backing_file.to_string_lossy(),
        &path.to_string_lossy(),
        &size_bytes.to_string(),
    ])
    .await
}

pub async fn clone_full(source: &Path, dest: &Path) -> Result<(), DiskError> {
    ensure_parent_dir_exists(dest).await?;

    run_qemu_img(&[
        "convert",
        "-f",
        "qcow2",
        "-O",
        "qcow2",
        &source.to_string_lossy(),
        &dest.to_string_lossy(),
    ])
    .await
}

pub async fn resize(path: &Path, new_size_bytes: u64, allow_shrink: bool) -> Result<(), DiskError> {
    let current_size_bytes = virtual_size_bytes(path).await?;

    if new_size_bytes < current_size_bytes && !allow_shrink {
        return Err(DiskError::ShrinkRequiresConfirmation {
            path: path.to_path_buf(),
            current_size_bytes,
            requested_size_bytes: new_size_bytes,
        });
    }

    let size_arg = new_size_bytes.to_string();
    let path_arg = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["resize"];
    if new_size_bytes < current_size_bytes {
        args.push("--shrink");
    }
    args.push(&path_arg);
    args.push(&size_arg);

    run_qemu_img(&args).await
}

pub async fn compact(path: &Path) -> Result<(), DiskError> {
    let disk_info = info(path).await?;
    if disk_info.format != "qcow2" {
        return Err(DiskError::CompactNotApplicable {
            path: path.to_path_buf(),
            format: disk_info.format,
        });
    }

    let tmp_path = path.with_extension("qcow2.compact-tmp");

    run_qemu_img(&[
        "convert",
        "-f",
        "qcow2",
        "-O",
        "qcow2",
        &path.to_string_lossy(),
        &tmp_path.to_string_lossy(),
    ])
    .await?;

    tokio::fs::rename(&tmp_path, path).await.map_err(|source| {
        let _ = std::fs::remove_file(&tmp_path);
        DiskError::Io {
            path: tmp_path,
            source,
        }
    })
}

pub async fn virtual_size_bytes(path: &Path) -> Result<u64, DiskError> {
    let output =
        run_qemu_img_capturing_stdout(&["info", "--output=json", &path.to_string_lossy()]).await?;

    parse_json_u64_field(&output, "virtual-size")
}

pub async fn disk_usage_bytes(path: &Path) -> Result<u64, DiskError> {
    let output =
        run_qemu_img_capturing_stdout(&["info", "--output=json", &path.to_string_lossy()]).await?;

    parse_json_u64_field(&output, "actual-size")
}

pub async fn info(path: &Path) -> Result<DiskInfo, DiskError> {
    let output =
        run_qemu_img_capturing_stdout(&["info", "--output=json", &path.to_string_lossy()]).await?;

    let virtual_size = parse_json_u64_field(&output, "virtual-size")?;
    let actual_size = parse_json_u64_field(&output, "actual-size")?;
    let format = parse_json_string_field(&output, "format")?;
    let backing_file = parse_json_optional_string_field(&output, "backing-filename");

    Ok(DiskInfo {
        virtual_size,
        actual_size,
        format,
        backing_file,
    })
}

async fn run_qemu_img(args: &[&str]) -> Result<(), DiskError> {
    run_qemu_img_capturing_stdout(args).await.map(|_| ())
}

async fn run_qemu_img_capturing_stdout(args: &[&str]) -> Result<String, DiskError> {
    let output = Command::new("qemu-img")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(DiskError::SpawnFailed)?;

    if !output.status.success() {
        return Err(DiskError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn ensure_parent_dir_exists(path: &Path) -> Result<(), DiskError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| DiskError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
    }
    Ok(())
}

fn parse_json_u64_field(json: &str, field_name: &str) -> Result<u64, DiskError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| DiskError::ParseError(format!("invalid JSON from qemu-img: {e}")))?;

    parsed
        .get(field_name)
        .ok_or_else(|| {
            DiskError::ParseError(format!("field `{field_name}` not found in qemu-img output"))
        })?
        .as_u64()
        .ok_or_else(|| {
            DiskError::ParseError(format!(
                "field `{field_name}` is not a valid u64 in qemu-img output"
            ))
        })
}

fn parse_json_string_field(json: &str, field_name: &str) -> Result<String, DiskError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| DiskError::ParseError(format!("invalid JSON from qemu-img: {e}")))?;

    let value = parsed
        .get(field_name)
        .ok_or_else(|| {
            DiskError::ParseError(format!("field `{field_name}` not found in qemu-img output"))
        })?
        .as_str()
        .ok_or_else(|| {
            DiskError::ParseError(format!(
                "field `{field_name}` is not a string in qemu-img output"
            ))
        })?;

    if value.is_empty() {
        return Err(DiskError::ParseError(format!(
            "field `{field_name}` is empty in qemu-img output"
        )));
    }

    Ok(value.to_string())
}

fn parse_json_optional_string_field(json: &str, field_name: &str) -> Option<String> {
    parse_json_string_field(json, field_name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_u64_field_extracts_value_compact() {
        let json = r#"{"virtual-size":42949672960,"actual-size":1234567}"#;
        assert_eq!(
            parse_json_u64_field(json, "virtual-size").unwrap(),
            42949672960
        );
        assert_eq!(parse_json_u64_field(json, "actual-size").unwrap(), 1234567);
    }

    #[test]
    fn parse_json_u64_field_extracts_value_spaced() {
        let json = r#"{"virtual-size": 42949672960, "actual-size": 1234567}"#;
        assert_eq!(
            parse_json_u64_field(json, "virtual-size").unwrap(),
            42949672960
        );
        assert_eq!(parse_json_u64_field(json, "actual-size").unwrap(), 1234567);
    }

    #[test]
    fn parse_json_u64_field_missing_field_is_parse_error() {
        let json = r#"{"virtual-size":42949672960}"#;
        let err = parse_json_u64_field(json, "actual-size").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_u64_field_zero_value() {
        let json = r#"{"virtual-size":0}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 0);
    }

    #[test]
    fn parse_json_u64_field_ignores_nested_field_with_same_name() {
        let json = r#"{"children":[{"info":{"virtual-size":197120,"format":"file"}}],"virtual-size":1048576,"format":"qcow2"}"#;
        let result = parse_json_u64_field(json, "virtual-size").unwrap();
        assert_eq!(
            result, 1048576,
            "must match the top-level field, not one nested inside `children`"
        );
    }

    #[test]
    fn parse_json_string_field_extracts_value_compact() {
        let json = r#"{"format":"qcow2","backing-filename":"/path/to/base.qcow2"}"#;
        assert_eq!(parse_json_string_field(json, "format").unwrap(), "qcow2");
        assert_eq!(
            parse_json_string_field(json, "backing-filename").unwrap(),
            "/path/to/base.qcow2"
        );
    }

    #[test]
    fn parse_json_string_field_extracts_value_spaced() {
        let json = r#"{"format": "qcow2", "backing-filename": "/path/to/base.qcow2"}"#;
        assert_eq!(parse_json_string_field(json, "format").unwrap(), "qcow2");
        assert_eq!(
            parse_json_string_field(json, "backing-filename").unwrap(),
            "/path/to/base.qcow2"
        );
    }

    #[test]
    fn parse_json_string_field_missing_field_is_error() {
        let json = r#"{"format":"qcow2"}"#;
        let err = parse_json_string_field(json, "backing-filename").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_string_field_empty_value_is_error() {
        let json = r#"{"format":""}"#;
        let err = parse_json_string_field(json, "format").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_string_field_empty_value_with_space_is_error() {
        let json = r#"{"format": ""}"#;
        let err = parse_json_string_field(json, "format").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_optional_string_field_returns_value_compact() {
        let json = r#"{"backing-filename":"/path/to/base.qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            Some("/path/to/base.qcow2".to_string())
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_value_spaced() {
        let json = r#"{"backing-filename": "/path/to/base.qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            Some("/path/to/base.qcow2".to_string())
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_none_when_missing() {
        let json = r#"{"format":"qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            None
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_none_when_empty() {
        let json = r#"{"backing-filename":""}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            None
        );
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_then_virtual_size_round_trips() {
        let dir = std::env::temp_dir().join("andler-disk-test-create");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 10 * 1024 * 1024 * 1024).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 10 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_with_backing_file_fails_fast_on_missing_backing() {
        let dir = std::env::temp_dir().join("andler-disk-test-missing-backing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let overlay_path = dir.join("overlay.qcow2");
        let missing_backing = dir.join("does-not-exist.qcow2");

        let err =
            create_with_backing_file(&overlay_path, &missing_backing, 10 * 1024 * 1024 * 1024)
                .await
                .unwrap_err();
        assert!(matches!(err, DiskError::BackingFileNotFound(_)));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_grow_succeeds_without_confirmation() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-grow");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 10 * 1024 * 1024 * 1024).await.unwrap();
        resize(&path, 20 * 1024 * 1024 * 1024, false).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 20 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_shrink_without_confirmation_is_rejected() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-shrink-reject");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 20 * 1024 * 1024 * 1024).await.unwrap();
        let err = resize(&path, 10 * 1024 * 1024 * 1024, false)
            .await
            .unwrap_err();
        assert!(matches!(err, DiskError::ShrinkRequiresConfirmation { .. }));
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 20 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_shrink_with_confirmation_succeeds() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-shrink-confirmed");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 20 * 1024 * 1024 * 1024).await.unwrap();
        resize(&path, 10 * 1024 * 1024 * 1024, true).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 10 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn compact_qcow2_succeeds() {
        let dir = std::env::temp_dir().join("andler-disk-test-compact-qcow2");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 1024 * 1024 * 1024).await.unwrap();
        compact(&path).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn compact_raw_is_rejected_as_not_applicable() {
        let dir = std::env::temp_dir().join("andler-disk-test-compact-raw");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.raw");

        run_qemu_img(&[
            "create",
            "-f",
            "raw",
            &path.to_string_lossy(),
            &(1024 * 1024 * 1024).to_string(),
        ])
        .await
        .unwrap();

        let err = compact(&path).await.unwrap_err();
        assert!(matches!(
            err,
            DiskError::CompactNotApplicable { format, .. } if format == "raw"
        ));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
