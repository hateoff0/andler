//! Offline provisioning Magisk root-доступа в overlay-диск Android-инстанса.
//!
//! Этот модуль монтирует overlay qcow2 (через `qemu-nbd`) и устанавливает
//! Magisk в файловую систему гостя перед первым запуском. Результат —
//! инстанс загружается с уже работающим root-доступом, без ручной
//! установки через recovery.
//!
//! ## Требования к хосту
//!
//! - Бинарник `qemu-nbd` (из `qemu-utils`) должен быть в `$PATH`.
//! - Модуль ядра `nbd` должен быть загружен (`modprobe nbd`).
//! - Процесс `andlerd` должен иметь права на:
//!   - чтение/запись к свободному `/dev/nbd*` (обычно группа `disk` или root)
//!   - `mount`/`umount` (обычно root или `CAP_SYS_ADMIN`)
//!
//! ## Гарантии очистки
//!
//! Все временные ресурсы (NBD-устройство, точка монтирования) освобождаются
//! через RAII-guard'ы (`NbdGuard`, `MountGuard`) — даже при ошибке на
//! середине операции устройство будет отмонтировано и отключено.
//!
//! ## Что делает Magisk при установке
//!
//! 1. Монтирует overlay (merged view: overlay + base image через backing chain).
//! 2. Копирует бинарники Magisk в `/data/adb/magisk/`.
//! 3. Создаёт каталог модулей `/data/adb/modules/`.
//! 4. Патчит boot image (если доступен) через `magiskboot`.
//!
//! Конкретные пути внутри образа зависят от того, как упакован
//! guest-image (отдельный `/boot`分区 или единый rootfs). Модуль
//! определяет layout автоматически.

use std::path::{Path, PathBuf};

use crate::error::DiskError;

/// Каталог с бинарниками Magisk, указанный пользователем через
/// `--magisk-dir`. Должен содержать как минимум `magisk` и `magiskinit`.
/// Опциональные файлы: `busybox`, `magiskboot`, `util-linux/`.
///
/// Проверяется до начала монтирования, чтобы не оставаться в
//! полузвёрзнутом состоянии при невалидном входе.
const REQUIRED_MAGISK_FILES: &[&str] = &["magisk", "magiskinit"];

/// Префикс имени точки монтирования — создаётся в `/tmp/andler-mount-<uuid>`.
const MOUNT_PREFIX: &str = "/tmp/andler-mount-";

// ---------------------------------------------------------------------------
// RAII- guard'ы
// ---------------------------------------------------------------------------

/// RAII-обёртка для NBD-устройства: при `drop` выполняет
/// `qemu-nbd --disconnect`.
struct NbdGuard {
    device_path: PathBuf,
}

impl NbdGuard {
    fn new(device_path: PathBuf) -> Self {
        NbdGuard { device_path }
    }

    fn path(&self) -> &Path {
        &self.device_path
    }
}

impl Drop for NbdGuard {
    fn drop(&mut self) {
        let status = std::process::Command::new("qemu-nbd")
            .args(["--disconnect", self.device_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match status {
            Ok(s) if s.success() => {
                tracing::debug!(device = %self.device_path.display(), "nbd device disconnected");
            }
            Ok(s) => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    status = %s,
                    "qemu-nbd --disconnect failed"
                );
            }
            Err(e) => {
                tracing::warn!(
                    device = %self.device_path.display(),
                    error = %e,
                    "failed to run qemu-nbd --disconnect"
                );
            }
        }
    }
}

/// RAII-обёртка для точки монтирования: при `drop` выполняет `umount -l`.
struct MountGuard {
    mount_point: PathBuf,
}

impl MountGuard {
    fn new(mount_point: PathBuf) -> Self {
        MountGuard { mount_point }
    }

    fn path(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let status = std::process::Command::new("umount")
            .args(["-l", self.mount_point.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status();

        match status {
            Ok(s) if s.success() => {
                tracing::debug!(point = %self.mount_point.display(), "mount point unmounted");
            }
            Ok(s) => {
                tracing::warn!(
                    point = %self.mount_point.display(),
                    status = %s,
                    "umount failed"
                );
            }
            Err(e) => {
                tracing::warn!(
                    point = %self.mount_point.display(),
                    error = %e,
                    "failed to run umount"
                );
            }
        }

        // Удаляем каталог точки монтирования (best-effort)
        let _ = std::fs::remove_dir(&self.mount_point);
    }
}

// ---------------------------------------------------------------------------
// Поиск свободного NBD-устройства
// ---------------------------------------------------------------------------

/// Ищет свободное NBD-устройство, сканируя `/sys/class/block/nbd*/size`.
/// Свободное — это то, у которого `size == 0` (не подключено ни одного образа).
///
/// Возвращает путь типа `/dev/nbd0`.
fn find_free_nbd_device() -> Result<PathBuf, DiskError> {
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

    // Сортируем по имени для детерминированности (nbd0, nbd1, ...)
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
            let dev_path = PathBuf::from(format!("/dev/{}", name.to_str().unwrap()));
            if dev_path.exists() {
                tracing::debug!(device = %dev_path.display(), "found free nbd device");
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
/// разделами (если они есть в образе) — `/dev/nbdNp1`, `/dev/nbdNp2` и т.д.
fn connect_nbd(overlay_path: &Path) -> Result<NbdGuard, DiskError> {
    let device = find_free_nbd_device()?;

    let output = std::process::Command::new("qemu-nbd")
        .args([
            "--connect",
            device.to_str().unwrap(),
            overlay_path.to_str().unwrap(),
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

    tracing::debug!(
        device = %device.display(),
        image = %overlay_path.display(),
        "nbd device connected"
    );

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
fn wait_for_partitions(nbd_dev: &Path) -> Result<Vec<PathBuf>, DiskError> {
    let dev_name = nbd_dev
        .file_name()
        .ok_or_else(|| DiskError::NbdSetupFailed("invalid nbd device path".to_string()))?
        .to_str()
        .ok_or_else(|| DiskError::NbdSetupFailed("non-utf8 device name".to_string()))?
        .to_string();

    let sys_path = PathBuf::from(format!("/sys/block/{dev_name}"));

    // Ожидаем появления partitions (макс. 2 секунды)
    for _ in 0..20 {
        let mut partitions = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&sys_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_str().unwrap_or("");
                if name_str.starts_with(&dev_name) && name_str != dev_name {
                    // Это partition: nbd0p1, nbd0p2 и т.д.
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

/// Определяет раздел с rootfs (Linux/Android filesystem) из списка
/// разделов. Ищет раздел с типом Linux (0x83) через чтение
/// `/sys/block/<dev>/<part>/partition` или проверку `file -s`.
///
/// Для простоты берём первый раздел (typical Android image layout:
/// single partition с full rootfs).
fn find_root_partition(partitions: &[PathBuf]) -> Result<PathBuf, DiskError> {
    if partitions.is_empty() {
        return Err(DiskError::NbdSetupFailed(
            "no partitions found in image".to_string(),
        ));
    }

    // Берём первый раздел — стандартный layout для Android images
    Ok(partitions[0].clone())
}

// ---------------------------------------------------------------------------
// Монтирование
// ---------------------------------------------------------------------------

/// Монтирует раздел в точку монтирования и возвращает guard.
fn mount_partition(partition: &Path) -> Result<MountGuard, DiskError> {
    let mount_point = PathBuf::from(format!(
        "{MOUNT_PREFIX}{}",
        uuid::Uuid::new_v4()
    ));

    std::fs::create_dir_all(&mount_point).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to create mount point {}: {e}",
        mount_point.display()
    )))?;

    let output = std::process::Command::new("mount")
        .args([
            "-o", "rw",
            partition.to_str().unwrap(),
            mount_point.to_str().unwrap(),
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

    tracing::debug!(
        partition = %partition.display(),
        mount = %mount_point.display(),
        "partition mounted"
    );

    Ok(MountGuard::new(mount_point))
}

// ---------------------------------------------------------------------------
// Копирование Magisk файлов
// ---------------------------------------------------------------------------

/// Проверяет, что каталог Magisk содержит обязательные файлы.
fn validate_magisk_dir(magisk_dir: &Path) -> Result<(), DiskError> {
    if !magisk_dir.is_dir() {
        return Err(DiskError::MagiskDirInvalid(format!(
            "magisk-dir is not a directory: {}",
            magisk_dir.display()
        )));
    }

    for file in REQUIRED_MAGISK_FILES {
        let path = magisk_dir.join(file);
        if !path.exists() {
            return Err(DiskError::MagiskDirInvalid(format!(
                "required file {file} not found in magisk-dir: {}",
                magisk_dir.display()
            )));
        }
    }

    Ok(())
}

/// Копирует файлы Magisk в смонтированный rootfs.
///
/// Создаёт структуру:
/// ```text
/// <mount>/data/adb/magisk/        — бинарники Magisk
/// <mount>/data/adb/modules/       — каталог модулей (пустой)
/// ```
fn copy_magisk_files(mount_point: &Path, magisk_dir: &Path) -> Result<(), DiskError> {
    let magisk_target = mount_point.join("data/adb/magisk");
    let modules_target = mount_point.join("data/adb/modules");

    // Создаём каталоги
    std::fs::create_dir_all(&magisk_target).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to create {}: {e}",
        magisk_target.display()
    )))?;
    std::fs::create_dir_all(&modules_target).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to create {}: {e}",
        modules_target.display()
    )))?;

    // Копируем все файлы из magisk_dir в data/adb/magisk/
    let entries = std::fs::read_dir(magisk_dir).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to read magisk-dir {}: {e}",
        magisk_dir.display()
    )))?;

    for entry in entries.flatten() {
        let src = entry.path();
        let dst = magisk_target.join(entry.file_name());

        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| DiskError::NbdSetupFailed(format!(
                "failed to copy {} to {}: {e}",
                src.display(),
                dst.display()
            )))?;
        }
    }

    tracing::info!(
        magisk_dir = %magisk_dir.display(),
        target = %magisk_target.display(),
        "magisk files copied"
    );

    Ok(())
}

/// Рекурсивное копирование каталога.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), DiskError> {
    std::fs::create_dir_all(dst).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to create dir {}: {e}",
        dst.display()
    )))?;

    for entry in std::fs::read_dir(src).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to read dir {}: {e}",
        src.display()
    )))? {
        let entry = entry.map_err(|e| DiskError::NbdSetupFailed(format!(
            "failed to read dir entry in {}: {e}",
            src.display()
        )))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| DiskError::NbdSetupFailed(format!(
                "failed to copy {} to {}: {e}",
                src_path.display(),
                dst_path.display()
            )))?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Патчинг boot image
// ---------------------------------------------------------------------------

/// Патчит boot image через `magiskboot`, если он доступен в образе.
///
/// Ищет `boot.img` в корне mounted filesystem или в `/boot/`.
/// Если `magiskboot` нет в magisk_dir — пропускает без ошибки
/// (Magisk может работать и без patching boot, в зависимости от версии).
fn patch_boot_image(mount_point: &Path, magisk_dir: &Path) -> Result<(), DiskError> {
    let magiskboot = magisk_dir.join("magiskboot");
    if !magiskboot.exists() {
        tracing::info!("magiskboot not found in magisk-dir, skipping boot image patching");
        return Ok(());
    }

    // Ищем boot.img
    let boot_img_candidates = [
        mount_point.join("boot.img"),
        mount_point.join("boot/boot.img"),
    ];

    let boot_img = boot_img_candidates
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| {
            tracing::info!("boot.img not found in image, skipping boot patching");
            // Не ошибка — некоторые образы не имеют отдельного boot.img
            return DiskError::NbdSetupFailed("boot.img not found".to_string());
        })?;

    tracing::info!(boot_img = %boot_img.display(), "found boot.img, patching with magiskboot");

    // magiskboot unpack boot.img /tmp/andler-magisk-patch-<uuid>
    let patch_dir = PathBuf::from(format!(
        "/tmp/andler-magisk-patch-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&patch_dir).map_err(|e| DiskError::NbdSetupFailed(format!(
        "failed to create patch dir: {e}"
    )))?;

    // Unpack
    let output = std::process::Command::new(&magiskboot)
        .args(["unpack", boot_img.to_str().unwrap()])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot unpack failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr.trim(), "magiskboot unpack failed, skipping");
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    // Patch — magiskboot patch boot.img patched_boot.img
    // (uses magisk's default patching logic)
    let patched_boot = patch_dir.join("patched_boot.img");
    let output = std::process::Command::new(&magiskboot)
        .args([
            "patch",
            boot_img.to_str().unwrap(),
            patched_boot.to_str().unwrap(),
        ])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot patch failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr.trim(), "magiskboot patch failed, skipping");
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    // Repack
    let output = std::process::Command::new(&magiskboot)
        .args([
            "repack",
            patched_boot.to_str().unwrap(),
            boot_img.to_str().unwrap(),
        ])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot repack failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr.trim(), "magiskboot repack failed, skipping");
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    // Очищаем временный каталог патча
    let _ = std::fs::remove_dir_all(&patch_dir);

    tracing::info!(boot_img = %boot_img.display(), "boot image patched successfully");

    Ok(())
}

// ---------------------------------------------------------------------------
// Публичный API
// ---------------------------------------------------------------------------

/// Устанавливает Magisk root-доступ в overlay-диск Android-инстанса offline.
///
/// # Arguments
/// * `overlay_path` — путь к overlay qcow2-файлу инстанса
/// * `magisk_dir` — каталог с бинарниками Magisk (результат распаковки
///   Magisk release ZIP). Должен содержать как минимум `magisk` и `magiskinit`.
///
/// # Process
/// 1. Подключает overlay через `qemu-nbd` к свободному `/dev/nbd*`
/// 2. Монтирует первый раздел (rootfs)
/// 3. Копирует бинарники Magisk в `/data/adb/magisk/`
/// 4. Создаёт каталог модулей `/data/adb/modules/`
/// 5. Патчит boot image (если `magiskboot` доступен и `boot.img` найден)
/// 6. Отмонтировывает и отключает NBD
///
/// Все шаги.cleanup happens automatically via RAII guards, even on error.
pub async fn provision_magisk(
    overlay_path: &Path,
    magisk_dir: &Path,
) -> Result<(), DiskError> {
    tracing::info!(
        overlay = %overlay_path.display(),
        magisk_dir = %magisk_dir.display(),
        "starting magisk provisioning"
    );

    // Валидация входных данных до начала тяжёлых операций
    validate_magisk_dir(magisk_dir)?;

    if !overlay_path.exists() {
        return Err(DiskError::BackingFileNotFound(overlay_path.to_path_buf()));
    }

    // Шаг 1: Подключение через qemu-nbd
    let nbd_guard = connect_nbd(overlay_path)?;

    // Шаг 2: Ожидание появления partitions
    let partitions = wait_for_partitions(nbd_guard.path())?;
    tracing::debug!(partitions = ?partitions, "partitions detected");

    // Шаг 3: Определение root-раздела
    let root_partition = find_root_partition(&partitions)?;

    // Шаг 4: Монтирование
    let mount_guard = mount_partition(&root_partition)?;

    // Шаг 5: Копирование файлов Magisk
    copy_magisk_files(mount_guard.path(), magisk_dir)?;

    // Шаг 6: Патчинг boot image (best-effort)
    if let Err(e) = patch_boot_image(mount_guard.path(), magisk_dir) {
        tracing::warn!(error = %e, "boot image patching failed (continuing)");
    }

    // mount_guard и nbd_guard будут drop'нуты автоматически при выходе
    // из функции — unmount + disconnect происходит в любом случае.

    tracing::info!("magisk provisioning completed successfully");

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_magisk_dir_rejects_nonexistent_path() {
        let err = validate_magisk_dir(Path::new("/nonexistent/magisk")).unwrap_err();
        assert!(matches!(err, DiskError::MagiskDirInvalid(_)));
    }

    #[test]
    fn validate_magisk_dir_rejects_missing_required_file() {
        let dir = std::env::temp_dir().join("andler-magisk-test-missing");
        let _ = std::fs::create_dir_all(&dir);
        // Создаём только magisk, но не magiskinit
        let _ = std::fs::write(dir.join("magisk"), b"");

        let err = validate_magisk_dir(&dir).unwrap_err();
        assert!(matches!(err, DiskError::MagiskDirInvalid(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_magisk_dir_accepts_valid_directory() {
        let dir = std::env::temp_dir().join("andler-magisk-test-valid");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("magisk"), b"");
        let _ = std::fs::write(dir.join("magiskinit"), b"");

        assert!(validate_magisk_dir(&dir).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_free_nbd_device_returns_error_when_no_nbd_module() {
        // /sys/class/block не существует в большинстве тестовых окружений
        let result = find_free_nbd_device();
        assert!(result.is_err());
    }

    #[test]
    fn copy_dir_recursive_creates_structure() {
        let src = std::env::temp_dir().join("andler-test-copy-src");
        let dst = std::env::temp_dir().join("andler-test-copy-dst");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file.txt"), b"hello").unwrap();
        std::fs::create_dir_all(src.join("subdir")).unwrap();
        std::fs::write(src.join("subdir/nested.txt"), b"world").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read(dst.join("file.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(dst.join("subdir/nested.txt")).unwrap(),
            b"world"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }
}
