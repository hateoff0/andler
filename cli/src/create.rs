use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use std::path::PathBuf;
use tonic::transport::Channel;

use crate::instance_file::{InstanceFile, InstanceFileResult};
use crate::{err_exit, CliAndroidVersion, CliKind, CliRootMode};

pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    file: Option<PathBuf>,
    kind: Option<CliKind>,
    name: Option<String>,
    ovmf_vars_template: Option<String>,
    iso_path: Option<String>,
    disk_path: Option<String>,
    disk_size_gib: Option<u64>,
    android_version: Option<CliAndroidVersion>,
    base_image_path: Option<String>,
    gapps: bool,
    microg: bool,
    libndk: bool,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_file = file.is_some();
    let has_kind = kind.is_some();

    if has_file && has_kind {
        err_exit("error: --file and --kind are mutually exclusive");
    }
    if !has_file && !has_kind {
        err_exit("error: specify either --file <path> or --kind linux|android");
    }

    if has_file {
        let file = file.unwrap();
        let instance_file = InstanceFile::load(&file)?;
        match instance_file.into_result() {
            InstanceFileResult::Linux(req) => {
                let response = client.create_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
            InstanceFileResult::Android(req) => {
                let response = client.create_android_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
        }
    } else {
        let kind = kind.unwrap();
        let name = name.unwrap_or_else(|| err_exit("error: --name is required"));
        let ovmf = ovmf_vars_template
            .unwrap_or_else(|| err_exit("error: --ovmf-vars-template is required"));

        match kind {
            CliKind::Linux => {
                let iso = iso_path
                    .unwrap_or_else(|| err_exit("error: --iso-path is required for --kind linux"));
                let disk = disk_path
                    .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));

                let req = build_linux_request(name, iso, disk, disk_size_gib, ovmf);
                let response = client.create_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
            CliKind::Android => {
                let av = android_version.unwrap_or_else(|| {
                    err_exit("error: --android-version is required for --kind android")
                });
                let bip = base_image_path.unwrap_or_else(|| {
                    err_exit("error: --base-image-path is required for --kind android")
                });

                if root == CliRootMode::Magisk && magisk_dir.is_none() {
                    err_exit("error: --magisk-dir is required when --root magisk");
                }

                let req = build_android_request(
                    name, av, bip, ovmf, gapps, microg, libndk, root,
                    instances_root, overlay_size_gib, magisk_dir,
                );
                let response = client.create_android_instance(req).await?;
                println!("{}", response.into_inner().instance_id);
            }
        }
    }

    Ok(())
}

fn build_linux_request(
    name: String,
    iso_path: String,
    disk_path: String,
    disk_size_gib: Option<u64>,
    ovmf_vars_template: String,
) -> CreateInstanceRequest {
    let mut disk = andler_core::DiskConfig::reference_default(std::path::PathBuf::from(&disk_path));
    if let Some(gib) = disk_size_gib {
        disk.size_bytes = gib.checked_mul(andler_core::DiskConfig::GIB)
            .expect("disk size overflow");
    }

    CreateInstanceRequest {
        name,
        iso_path,
        cpu: Some(andler_core::CpuConfig::reference_default().into()),
        memory: Some(andler_core::MemoryConfig::reference_default().into()),
        disk: Some(disk.into()),
        display: Some(andler_core::DisplayConfig::reference_default().into()),
        gpu: Some(andler_core::GpuConfig::reference_default().into()),
        network: Some(andler_core::NetworkConfig::reference_default().into()),
        firmware: Some(
            andler_core::FirmwareConfig::reference_default(std::path::PathBuf::from(
                &ovmf_vars_template,
            ))
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
    }
}

fn build_android_request(
    name: String,
    android_version: CliAndroidVersion,
    base_image_path: String,
    ovmf_vars_template: String,
    gapps: bool,
    microg: bool,
    libndk: bool,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> CreateAndroidInstanceRequest {
    let mut profile = ProtoAndroidProfile {
        gapps,
        microg,
        libndk,
        ..Default::default()
    };
    profile.set_android_version(android_version.into());
    profile.set_root(root.into());

    CreateAndroidInstanceRequest {
        name,
        profile: Some(profile),
        base_image_path,
        instances_root,
        overlay_size_bytes: overlay_size_gib.checked_mul(1024 * 1024 * 1024)
            .expect("overlay size overflow"),
        ovmf_vars_template,
        magisk_dir: magisk_dir
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}
