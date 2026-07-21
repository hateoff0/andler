

use std::path::{Path, PathBuf};

use crate::error::DiskError;
use crate::qcow2;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClonedDisk {
    pub disk_path: PathBuf,

    pub backing_file: Option<PathBuf>,
}


pub async fn linked_clone(
    source_disk_path: &Path,
    dest_path: &Path,
    size_bytes: u64,
) -> Result<ClonedDisk, DiskError> {
    qcow2::create_with_backing_file(dest_path, source_disk_path, size_bytes).await?;

    Ok(ClonedDisk {
        disk_path: dest_path.to_path_buf(),
        backing_file: Some(source_disk_path.to_path_buf()),
    })
}


pub async fn full_standalone_clone(
    source_disk_path: &Path,
    dest_path: &Path,
) -> Result<ClonedDisk, DiskError> {
    qcow2::clone_full(source_disk_path, dest_path).await?;

    Ok(ClonedDisk {
        disk_path: dest_path.to_path_buf(),
        backing_file: None,
    })
}


pub async fn shared_base_clone(
    source_disk_path: &Path,
    dest_path: &Path,
    source_base_image: &Path,
) -> Result<ClonedDisk, DiskError> {
    if let Some(parent) = dest_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| DiskError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }

    tokio::fs::copy(source_disk_path, dest_path)
        .await
        .map_err(|source| DiskError::Io {
            path: dest_path.to_path_buf(),
            source,
        })?;

    Ok(ClonedDisk {
        disk_path: dest_path.to_path_buf(),
        backing_file: Some(source_base_image.to_path_buf()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;


    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn linked_clone_points_at_source_instance_disk() {
        let dir = std::env::temp_dir().join("andler-disk-test-linked-clone");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let source_instance_dir = dir.join("instance-a");
        tokio::fs::create_dir_all(&source_instance_dir).await.unwrap();
        let source_disk = source_instance_dir.join("disk.qcow2");
        qcow2::create_with_backing_file(&source_disk, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let dest_instance_dir = dir.join("instance-b");
        let dest_disk = dest_instance_dir.join("disk.qcow2");

        let result = linked_clone(&source_disk, &dest_disk, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        assert_eq!(result.disk_path, dest_disk);
        assert_eq!(result.backing_file, Some(source_disk.clone()));
        assert!(dest_disk.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn full_standalone_clone_has_no_backing_file() {
        let dir = std::env::temp_dir().join("andler-disk-test-standalone-clone");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let source_instance_dir = dir.join("instance-a");
        tokio::fs::create_dir_all(&source_instance_dir).await.unwrap();
        let source_disk = source_instance_dir.join("disk.qcow2");
        qcow2::create_with_backing_file(&source_disk, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let dest_disk = dir.join("instance-b").join("disk.qcow2");

        let result = full_standalone_clone(&source_disk, &dest_disk).await.unwrap();

        assert_eq!(result.backing_file, None);
        assert!(dest_disk.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn shared_base_clone_does_not_depend_on_source_after_copy() {
        let dir = std::env::temp_dir().join("andler-disk-test-shared-base-clone");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let source_instance_dir = dir.join("instance-a");
        tokio::fs::create_dir_all(&source_instance_dir).await.unwrap();
        let source_disk = source_instance_dir.join("disk.qcow2");
        qcow2::create_with_backing_file(&source_disk, &base_image, 20 * 1024 * 1024 * 1024)
            .await
            .unwrap();

        let dest_disk = dir.join("instance-b").join("disk.qcow2");

        let result = shared_base_clone(&source_disk, &dest_disk, &base_image)
            .await
            .unwrap();

        assert_eq!(result.backing_file, Some(base_image.clone()));
        assert!(dest_disk.exists());

        tokio::fs::remove_dir_all(&source_instance_dir).await.unwrap();
        assert!(dest_disk.exists());
        assert!(qcow2::virtual_size_bytes(&dest_disk).await.is_ok());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn shared_base_clone_reports_missing_source_as_io_error() {
        let dir = std::env::temp_dir().join("andler-disk-test-shared-base-missing-source");
        let missing_source = dir.join("does-not-exist.qcow2");
        let dest = dir.join("dest.qcow2");
        let fake_base = dir.join("base.qcow2");

        let err = shared_base_clone(&missing_source, &dest, &fake_base)
            .await
            .unwrap_err();
        assert!(matches!(err, DiskError::Io { .. }));
    }
}
