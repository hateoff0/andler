use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, CreateAndroidInstanceRequest, CreateInstanceRequest,
};
use std::path::PathBuf;
use tonic::transport::Channel;

use crate::instance_file::{InstanceFile, InstanceFileResult};
use crate::wizard::{PartialArgs, WizardError, WizardKind, WizardResult};
use crate::{err_exit, CliAndroidVersion, CliArmTranslator, CliCdromBus, CliKind, CliRootMode};

#[allow(clippy::too_many_arguments)]
pub async fn handle(
    client: &mut AndlerServiceClient<Channel>,
    file: Option<PathBuf>,
    kind: Option<CliKind>,
    name: Option<String>,
    ovmf_vars_template: Option<String>,
    iso_path: Option<String>,
    disk_path: Option<String>,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    advanced: bool,
    android_version: Option<CliAndroidVersion>,
    base_image_path: Option<String>,
    gapps: bool,
    microg: bool,
    arm_translator: Option<CliArmTranslator>,
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

    // --- TOML-режим ---
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
        return Ok(());
    }

    // --- Wizard-режим: запускается, если не переданы обязательные параметры ---
    let linux_required = kind == Some(CliKind::Linux) && (name.is_none() || iso_path.is_none() || disk_path.is_none());
    let android_required = kind == Some(CliKind::Android) && (name.is_none() || base_image_path.is_none());
    let no_kind = kind.is_none();
    let needs_wizard = no_kind || linux_required || android_required;

    if needs_wizard {
        let partial = PartialArgs {
            kind: kind.map(|k| match k {
                CliKind::Linux => WizardKind::Linux,
                CliKind::Android => WizardKind::Android,
            }),
            name: name.clone(),
            iso_path: iso_path.clone(),
            base_image_path: base_image_path.clone(),
            instances_root: Some(instances_root.clone()),
        };

        let result = crate::wizard::run(partial, advanced).await;

        return match result {
            Ok(WizardResult::Linux(req, root)) => {
                let response = client.create_instance(req).await?;
                let id = response.into_inner().instance_id;
                println!("✓ VM создана: {id}");
                println!("  andler start {id}");
                Ok(())
            }
            Ok(WizardResult::Android(req)) => {
                let response = client.create_android_instance(req).await?;
                let id = response.into_inner().instance_id;
                println!("✓ VM создана: {id}");
                println!("  andler start {id}");
                Ok(())
            }
            Err(WizardError::Cancelled) => {
                println!("Отменено.");
                Ok(())
            }
            Err(WizardError::NotTty) => {
                eprintln!("{}", WizardError::NotTty);
                std::process::exit(1);
            }
            Err(e) => Err(e.into()),
        };
    }

    // --- CLI-режим (все обязательные параметры переданы) ---
    let kind = kind.unwrap();
    let name = name.unwrap();
    let ovmf = ovmf_vars_template.unwrap_or_default(); // daemon auto-detects if empty

    match kind {
        CliKind::Linux => {
            let iso = iso_path
                .unwrap_or_else(|| err_exit("error: --iso-path is required for --kind linux"));
            let disk = disk_path
                .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));

            let req = build_linux_request(
                name,
                iso,
                disk,
                disk_size_gib,
                compact_on_shutdown,
                cdrom_bus,
                ovmf,
            );
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

            // В чистом CLI-режиме (без wizard) авто-детект по CPU не
            // запускается — не заданный флаг значит "без транслятора",
            // явно и предсказуемо. Авто-детект — привилегия wizard'а
            // (см. `andler-firmware::detect::arm`).
            let arm_translator = arm_translator.unwrap_or(CliArmTranslator::None);

            let req = build_android_request(
                name, av, bip, ovmf, gapps, microg, arm_translator, root,
                instances_root, overlay_size_gib, magisk_dir,
            );
            let response = client.create_android_instance(req).await?;
            println!("{}", response.into_inner().instance_id);
        }
    }

    Ok(())
}

fn build_linux_request(
    name: String,
    iso_path: String,
    disk_path: String,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    ovmf_vars_template: String,
) -> CreateInstanceRequest {
    let mut disk = andler_core::DiskConfig::reference_default(std::path::PathBuf::from(&disk_path));
    if let Some(gib) = disk_size_gib {
        disk.size_bytes = gib.checked_mul(andler_core::DiskConfig::GIB)
            .expect("disk size overflow");
    }
    disk.compact_on_shutdown = compact_on_shutdown;

    // `auto` — не значение `CdromBus` само по себе, а просьба применить
    // эвристику по имени ISO-файла (см. PLAN.md, раздел «Монтирование
    // ISO / CD-ROM» → «Правило выбора дефолта»); явный выбор
    // пользователя (`--cdrom-bus virtio|ide`) этой эвристикой не
    // переопределяется.
    let resolved_cdrom_bus = match cdrom_bus {
        CliCdromBus::Auto => andler_core::CdromBus::recommended_for_iso_filename(
            std::path::Path::new(&iso_path),
        ),
        CliCdromBus::Virtio => andler_core::CdromBus::VirtioScsi,
        CliCdromBus::Ide => andler_core::CdromBus::Ide,
    };

    let mut req = CreateInstanceRequest {
        name,
        iso_path,
        cpu: Some(andler_core::CpuConfig::reference_default().into()),
        memory: Some(andler_core::MemoryConfig::reference_default().into()),
        disk: Some(disk.into()),
        display: Some(andler_core::DisplayConfig::reference_default().into()),
        gpu: Some(andler_core::GpuConfig::reference_default().into()),
        network: Some(andler_core::NetworkConfig::reference_default().into()),
        firmware: Some(
            andler_core::FirmwareConfig {
                ovmf_code_path: std::path::PathBuf::new(), // daemon подставит авто-определённый
                ovmf_vars_path: std::path::PathBuf::from(&ovmf_vars_template),
            }
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
        ..Default::default()
    };
    req.set_cdrom_bus(resolved_cdrom_bus.into());
    req
}

fn build_android_request(
    name: String,
    android_version: CliAndroidVersion,
    base_image_path: String,
    ovmf_vars_template: String,
    gapps: bool,
    microg: bool,
    arm_translator: CliArmTranslator,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> CreateAndroidInstanceRequest {
    let mut profile = ProtoAndroidProfile {
        gapps,
        microg,
        ..Default::default()
    };
    profile.set_android_version(android_version.into());
    profile.set_root(root.into());
    profile.set_arm_translator(arm_translator.into());

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
