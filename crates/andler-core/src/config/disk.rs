//! Конфигурация диска инстанса.
//!
//! Источник истины — `scripts/start.sh`:
//! `-drive file=disk.qcow2,format=qcow2,if=none,discard=on,detect-zeroes=on,aio=threads`
//! + `-device virtio-blk-pci,...,num-queues=4`. Для Android-инстансов —
//! `base_image`/overlay-механизм из §4.4.2
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Формат файла диска.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskFormat {
    Qcow2,
    Raw,
    Vdi,
}

/// Конфигурация диска инстанса.
///
/// Для `InstanceKind::LinuxVm` обычно `base_image: None` — диск создаётся
/// с нуля через `andler-disk::qcow2::create`. Для `InstanceKind::AndroidVm`
/// `base_image` указывает на закэшированный/скачанный базовый образ
/// (см. §4.4.2 архитектурного плана), а `path` — на overlay-диск конкретного
/// инстанса с `backing_file = base_image`; базовый образ при этом никогда
/// не модифицируется напрямую (`andler-disk::overlay`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskConfig {
    /// Путь к файлу диска, который реально подключается к QEMU
    /// (`-drive file=...`). Для Android-инстанса это overlay, а не
    /// базовый образ.
    pub path: PathBuf,
    /// Логический размер диска в байтах (виртуальный размер, не
    /// фактический занятый объём на хосте — для thin-provisioned дисков
    /// это разные числа).
    pub size_bytes: u64,
    pub format: DiskFormat,
    /// Базовый образ (`backing_file`) для linked-clone/overlay-дисков.
    /// `None` — обычный самостоятельный диск без backing chain.
    pub base_image: Option<PathBuf>,
    /// Растить файл по мере фактического использования (`qemu-img create`
    /// без предварительной полной аллокации). В `start.sh` подразумевается
    /// неявно самим форматом `qcow2`.
    pub thin_provisioning: bool,
    /// Выполнять TRIM/discard при штатном завершении инстанса —
    /// освобождает место на хосте от удалённых внутри гостя данных.
    /// Соответствует `discard=on,detect-zeroes=on` в `start.sh`, хотя там
    /// это включено постоянно при работе диска, а не только при shutdown;
    /// разделение на runtime-discard и shutdown-trim — решение, которое
    /// `andler-qemu` принимает на основе этого флага плюс самого факта
    /// `discard=on` устройства (см. TODO в `andler-qemu` README).
    pub trim_on_shutdown: bool,
}

impl DiskConfig {
    pub const GIB: u64 = 1024 * 1024 * 1024;

    /// Конфигурация обычного самостоятельного диска (`LinuxVm`),
    /// соответствующая `start.sh`: `disk.qcow2`, 40 GiB, qcow2,
    /// без backing image, thin-provisioned, discard включён.
    pub fn reference_default(path: PathBuf) -> Self {
        DiskConfig {
            path,
            size_bytes: 40 * Self::GIB,
            format: DiskFormat::Qcow2,
            base_image: None,
            thin_provisioning: true,
            trim_on_shutdown: true,
        }
    }

    /// Конфигурация overlay-диска для Android-инстанса: тот же набор
    /// флагов, что у `reference_default`, но с явно заданным
    /// `base_image` — overlay всегда thin-provisioned и qcow2, так как
    /// backing-chain в QEMU поддерживается только для qcow2.
    /// См. §4.4.2 архитектурного плана.
    pub fn overlay(path: PathBuf, base_image: PathBuf, size_bytes: u64) -> Self {
        DiskConfig {
            path,
            size_bytes,
            format: DiskFormat::Qcow2,
            base_image: Some(base_image),
            thin_provisioning: true,
            trim_on_shutdown: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = DiskConfig::reference_default(PathBuf::from("disk.qcow2"));
        assert_eq!(cfg.size_bytes, 40 * DiskConfig::GIB);
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert_eq!(cfg.base_image, None);
        assert!(cfg.thin_provisioning);
        assert!(cfg.trim_on_shutdown);
    }

    #[test]
    fn overlay_points_at_base_image() {
        let base = PathBuf::from("/var/lib/andler/images/android-13-gapps.qcow2");
        let cfg = DiskConfig::overlay(
            PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2"),
            base.clone(),
            20 * DiskConfig::GIB,
        );
        assert_eq!(cfg.base_image, Some(base));
        assert_eq!(cfg.format, DiskFormat::Qcow2);
    }
}
