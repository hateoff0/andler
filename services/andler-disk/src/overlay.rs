

use std::path::{Path, PathBuf};

use crate::error::DiskError;
use crate::qcow2;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayDisk {
    pub overlay_path: PathBuf,
    pub base_image_path: PathBuf,
}


pub async fn create_overlay(
    instance_dir: &Path,
    base_image_path: &Path,
    overlay_size_bytes: u64,
) -> Result<OverlayDisk, DiskError> {
    let overlay_path = instance_dir.join("disk.qcow2");

    qcow2::create_with_backing_file(&overlay_path, base_image_path, overlay_size_bytes).await?;

    Ok(OverlayDisk {
        overlay_path,
        base_image_path: base_image_path.to_path_buf(),
    })
}


pub async fn factory_reset(
    instance_dir: &Path,
    base_image_path: &Path,
    overlay_size_bytes: u64,
) -> Result<OverlayDisk, DiskError> {
    let overlay_path = instance_dir.join("disk.qcow2");

    if overlay_path.exists() {
        tokio::fs::remove_file(&overlay_path)
            .await
            .map_err(|source| DiskError::Io {
                path: overlay_path.clone(),
                source,
            })?;
    }

    create_overlay(instance_dir, base_image_path, overlay_size_bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;


    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_overlay_points_at_given_base_image() {
        let dir = std::env::temp_dir().join("andler-disk-test-overlay");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let instance_dir = dir.join("instance-abc");
        tokio::fs::create_dir_all(&instance_dir).await.unwrap();

        let result = create_overlay(&instance_dir, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        assert_eq!(result.overlay_path, instance_dir.join("disk.qcow2"));
        assert_eq!(result.base_image_path, base_image);
        assert!(result.overlay_path.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_overlay_fails_when_base_image_missing() {
        let dir = std::env::temp_dir().join("andler-disk-test-overlay-missing-base");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let missing_base = dir.join("does-not-exist.qcow2");
        let instance_dir = dir.join("instance-abc");

        let err = create_overlay(&instance_dir, &missing_base, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap_err();
        assert!(matches!(err, DiskError::BackingFileNotFound(_)));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn factory_reset_recreates_overlay() {
        let dir = std::env::temp_dir().join("andler-disk-test-factory-reset");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let instance_dir = dir.join("instance-abc");
        tokio::fs::create_dir_all(&instance_dir).await.unwrap();

        create_overlay(&instance_dir, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let overlay_path = instance_dir.join("disk.qcow2");
        assert!(overlay_path.exists());

        let result = factory_reset(&instance_dir, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        assert!(result.overlay_path.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
