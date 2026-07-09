//! Общие утилиты для работы с NBD-устройствами и монтированием разделов.
//!
//! Переиспользуются модулями `magisk` (offline provisioning Magisk) и
//! `guest_tools` (offline установка/удаление пакетов в гостевую ФС).
//!
//! ## Требования к хосту
//!
//! - Бинарник `qemu-nbd` (из `qemu-utils`) должен быть в `$PATH`.
//! - Модуль ядра `nbd` должен быть загружен (`modprobe nbd`).
//! - Процесс `andlerd` должен иметь права на:
//!   - чтение/запись к свободному `/dev/nbd*` (обычно группа `disk` или root)
//!   - `mount`/`umount` (обычно root или `CAP_SYS_ADMIN`)

use std::path::{Path, PathBuf};

use crate::error::DiskError;

/// Базовая директория для точек монтирования — подкаталог
/// `andler-mounts` под `andler_core::paths::runtime_dir()`, не голый
/// `/tmp` (world-writable на многопользовательской системе, уязвим к
/// symlink-атаке — см. PLAN.md, item 20a).
fn mount_dir_base() -> PathBuf {
    andler_core::paths::runtime_dir().join("andler-mounts")
}

// ---------------------------------------------------------------------------
// RAII- guard'ы
// ---------------------------------------------------------------------------

/// RAII-обёртка для NBD-устройства: при `drop` выполняет
/// `qemu-nbd --disconnect`.
pub struct NbdGuard {
    device_path: PathBuf,
}

impl NbdGuard {
    pub fn new(device_path: PathBuf) -> Self {
        NbdGuard { device_path }
    }

    pub fn path(&self) -> &Path {
        &self.device_path
    }
}

impl Drop for NbdGuard {
    fn drop(&mut self) {
        let result = std::process::Command::new("qemu-nbd")
            .args(["--disconnect", &self.device_path.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match result {
            Ok(status) if !status.success() => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    exit_status = %status,
                    "qemu-nbd --disconnect exited with a non-zero status; \
                     /dev/nbd* device may remain connected"
                );
            }
            Err(error) => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    %error,
                    "failed to spawn qemu-nbd --disconnect; \
                     /dev/nbd* device may remain connected"
                );
            }
            Ok(_) => {}
        }
    }
}

/// RAII-обёртка для точки монтирования: при `drop` выполняет `umount -l`.
pub struct MountGuard {
    mount_point: PathBuf,
}

impl MountGuard {
    pub fn new(mount_point: PathBuf) -> Self {
        MountGuard { mount_point }
    }

    pub fn path(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let result = std::process::Command::new("umount")
            .args(["-l", &self.mount_point.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match result {
            Ok(status) if !status.success() => {
                tracing::warn!(
                    mount_point = %self.mount_point.display(),
                    exit_status = %status,
                    "umount -l exited with a non-zero status"
                );
            }
            Err(error) => {
                tracing::warn!(
                    mount_point = %self.mount_point.display(),
                    %error,
                    "failed to spawn umount -l"
                );
            }
            Ok(_) => {}
        }

        if let Err(error) = std::fs::remove_dir(&self.mount_point) {
            tracing::warn!(
                mount_point = %self.mount_point.display(),
                %error,
                "failed to remove mount point directory"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Поиск свободного NBD-устройства
// ---------------------------------------------------------------------------

/// Ищет свободное NBD-устройство, сканируя `/sys/class/block/nbd*/size`.
/// Свободное — это то, у которого `size == 0` (не подключено ни одного образа).
///
/// Возвращает путь типа `/dev/nbd0`.
pub fn find_free_nbd_device() -> Result<PathBuf, DiskError> {
    let sys_block = Path::new("/sys/class/block");

    if !sys_block.exists() {
        return Err(DiskError::NbdSetupFailed(
            "/sys/class/block does not exist — nbd kernel module not loaded? \
             Run: sudo modprobe nbd"
                .to_string(),
        ));
    }

    let mut entries: Vec<_> = std::fs::read_dir(sys_block)
        .map_err(|e| DiskError::NbdSetupFailed(format!("read /sys/class/block: {e}")))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map_or(false, |name| name.starts_with("nbd"))
        })
        .collect();

    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let size_path = entry.path().join("size");
        let size_str = match std::fs::read_to_string(&size_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let size: u64 = match size_str.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        if size == 0 {
            let name = entry.file_name();
            let dev_path = PathBuf::from(format!("/dev/{}", name.to_string_lossy()));
            if dev_path.exists() {
                return Ok(dev_path);
            }
        }
    }

    Err(DiskError::NbdSetupFailed(
        "no free nbd device found — all /dev/nbd* are in use \
         or nbd module is not loaded"
            .to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Подключение образа через qemu-nbd
// ---------------------------------------------------------------------------

/// Подключает qcow2-образ к NBD-устройству и возвращает guard.
///
/// После вызова `/dev/nbdN` доступен как блочное устройство с
/// разделами — `/dev/nbdNp1`, `/dev/nbdNp2` и т.д.
pub fn connect_nbd(overlay_path: &Path) -> Result<NbdGuard, DiskError> {
    let device = find_free_nbd_device()?;

    let device_str = device.to_string_lossy().into_owned();
    let overlay_str = overlay_path.to_string_lossy().into_owned();

    let output = std::process::Command::new("qemu-nbd")
        .args([
            "--connect",
            &device_str,
            &overlay_str,
            "--format=qcow2",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run qemu-nbd: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DiskError::NbdSetupFailed(format!(
            "qemu-nbd --connect failed (exit {}): {}",
            output.status,
            stderr.trim()
        )));
    }

    Ok(NbdGuard::new(device))
}

/// Ждёт появления partition-устройств для NBD-девайса.
///
/// После `qemu-nbd --connect` ядру может потребоваться время, чтобы
/// просканировать таблицу разделов и создать `/dev/nbdNp1`. Функция
/// опрашивает `/sys/block/<dev>/` до появления первого `nbdNp*` entry
/// или таймаута (2 секунды).
///
/// Возвращает список найденных partition-устройств.
pub fn wait_for_partitions(nbd_dev: &Path) -> Result<Vec<PathBuf>, DiskError> {
    let dev_name = nbd_dev
        .file_name()
        .ok_or_else(|| DiskError::NbdSetupFailed("invalid nbd device path".to_string()))?
        .to_str()
        .ok_or_else(|| DiskError::NbdSetupFailed("non-utf8 device name".to_string()))?
        .to_string();

    let sys_path = PathBuf::from(format!("/sys/block/{dev_name}"));

    for _ in 0..20 {
        let mut partitions = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&sys_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_str().unwrap_or("");
                if name_str.starts_with(&dev_name) && name_str != dev_name {
                    let part_path = PathBuf::from(format!("/dev/{name_str}"));
                    if part_path.exists() {
                        partitions.push(part_path);
                    }
                }
            }
        }

        if !partitions.is_empty() {
            partitions.sort();
            return Ok(partitions);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(DiskError::NbdSetupFailed(format!(
        "partitions did not appear on {dev_name} within timeout"
    )))
}

/// Определяет раздел с rootfs из списка разделов.
///
/// Для простоты берём первый раздел (typical image layout: single partition
/// с rootfs).
pub fn find_root_partition(partitions: &[PathBuf]) -> Result<PathBuf, DiskError> {
    if partitions.is_empty() {
        return Err(DiskError::NbdSetupFailed(
            "no partitions found in image".to_string(),
        ));
    }

    Ok(partitions[0].clone())
}

// ---------------------------------------------------------------------------
// Монтирование
// ---------------------------------------------------------------------------

/// Генерирует уникальное имя для точки монтирования.
pub fn unique_mount_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}-{}", std::process::id(), id, ts)
}

/// Монтирует раздел в точку монтирования и возвращает guard.
pub fn mount_partition(partition: &Path) -> Result<MountGuard, DiskError> {
    let mount_point = mount_dir_base().join(unique_mount_name());

    andler_core::paths::ensure_private_dir_sync(&mount_point).map_err(|e| {
        DiskError::NbdSetupFailed(format!(
            "failed to create mount point {}: {e}",
            mount_point.display()
        ))
    })?;

    let partition_str = partition.to_string_lossy().into_owned();
    let mount_point_str = mount_point.to_string_lossy().into_owned();

    let output = std::process::Command::new("mount")
        .args([
            "-o", "rw",
            &partition_str,
            &mount_point_str,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to run mount: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir(&mount_point);
        return Err(DiskError::NbdSetupFailed(format!(
            "mount {} on {} failed: {}",
            partition.display(),
            mount_point.display(),
            stderr.trim()
        )));
    }

    Ok(MountGuard::new(mount_point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_free_nbd_device_returns_error_when_no_nbd_module() {
        let result = find_free_nbd_device();
        assert!(result.is_err());
    }

    #[test]
    fn unique_mount_name_is_unique() {
        let a = unique_mount_name();
        let b = unique_mount_name();
        assert_ne!(a, b);
    }
}
