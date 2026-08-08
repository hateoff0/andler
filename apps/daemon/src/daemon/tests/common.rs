use super::*;
pub(crate) use andler_core::AndroidVersion;
use andler_core::{
    AndroidProfile, ArmTranslator, AudioConfig, CdromBus, CpuConfig, DiskConfig, DisplayConfig,
    FirmwareConfig, GpuConfig, InputConfig, InstanceKind, MemoryConfig, NetworkConfig,
};
use std::path::PathBuf;

pub(crate) struct TestTempDir(PathBuf);

impl TestTempDir {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "andler-daemon-test-{}-{}",
            std::process::id(),
            InstanceId::new().to_string()
        ));
        std::fs::create_dir_all(&path).expect("create test temp dir");
        TestTempDir(path)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn sample_config() -> InstanceConfig {
    let disk_path = PathBuf::from("/tmp/test-disk.qcow2");
    let vars_path = PathBuf::from("/tmp/test-vars.fd");
    let _ = std::fs::File::create(&disk_path);
    let _ = std::fs::File::create(&vars_path);

    InstanceConfig {
        id: InstanceId::new(),
        name: "test-vm".to_string(),
        kind: InstanceKind::LinuxVm {
            iso_path: PathBuf::from("/tmp/test.iso"),
            cdrom_bus: CdromBus::Ide,
        },
        backend: BackendKind::Qemu,
        schema_version: andler_core::CURRENT_SCHEMA_VERSION,
        cpu: CpuConfig::reference_default(),
        memory: MemoryConfig::reference_default(),
        disk: DiskConfig::reference_default(disk_path),
        display: DisplayConfig::reference_default(),
        gpu: GpuConfig::reference_default(),
        network: NetworkConfig::reference_default(),
        extra_disks: Vec::new(),
        extra_networks: Vec::new(),
        firmware: FirmwareConfig::reference_default(vars_path),
        audio: AudioConfig::reference_default(),
        input: InputConfig::reference_default(),
    }
}

pub(crate) fn sample_android_config(disk_path: PathBuf, base_image: PathBuf) -> InstanceConfig {
    let mut cfg = sample_config();
    cfg.id = InstanceId::new();
    cfg.kind = InstanceKind::AndroidVm {
        android_profile: AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            arm_translator: ArmTranslator::None,
        },
    };
    cfg.disk = DiskConfig::overlay(disk_path, base_image, 20 * DiskConfig::GIB);
    cfg
}
