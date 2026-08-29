use std::io::Error as IoError;
use std::path::PathBuf;

use super::error::DaemonError;
use super::Daemon;
use andler_core::{
    oci_export::{build_layer, ImageSpec, OciExportError},
    DiskFormat, InstanceId,
};

impl Daemon {
    /// Exports an instance's disk as an OCI image layout.
    ///
    /// Resolves the source disk (the instance's configured disk, or the
    /// override), converts the qcow2 source to `disk_format` at a temp path,
    /// streams the converted image into the blob store, and assembles a
    /// conformant OCI layout at `dest_path`. Streaming (rather than loading
    /// the whole image into memory) keeps export working for large disks. The
    /// architecture is `amd64` because the backend is x86_64-only.
    pub async fn export_instance_oci(
        &self,
        source_id: InstanceId,
        dest_path: PathBuf,
        disk_format: DiskFormat,
        disk_path: Option<PathBuf>,
    ) -> Result<(), DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;

        let disk_path = disk_path.unwrap_or_else(|| source_config.disk.path.clone());

        let info = andler_disk::qcow2::info(&disk_path)
            .await
            .map_err(DaemonError::Disk)?;
        if info.format != "qcow2" {
            return Err(DaemonError::ExportRequiresQcow2 {
                instance_id: source_id,
                format: info.format,
            });
        }

        let temp_path = std::env::temp_dir().join(format!("andler-export-{source_id}.tmp"));
        andler_disk::qcow2::convert(&disk_path, &temp_path, disk_format)
            .await
            .map_err(DaemonError::Disk)?;

        let closure_temp = temp_path.clone();
        let closure_dest = dest_path.clone();
        let spec = ImageSpec::new("amd64");
        tokio::task::spawn_blocking(move || {
            let layer = std::fs::File::open(&closure_temp).map_err(|source| DaemonError::Io {
                path: closure_temp.clone(),
                source,
            })?;
            build_layer(&closure_dest, &spec, layer).map_err(|source| DaemonError::OciExport {
                path: closure_dest.clone(),
                source,
            })
        })
        .await
        .map_err(|join_err| DaemonError::OciExport {
            path: dest_path.clone(),
            source: OciExportError::Io {
                path: temp_path.clone(),
                source: IoError::other(format!("export task failed: {join_err}")),
            },
        })??;

        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok(())
    }
}
