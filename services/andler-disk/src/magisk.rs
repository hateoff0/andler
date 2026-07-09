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
//! guest-image (отдельный `/boot`-раздел или единый rootfs). Модуль
//! определяет layout автоматически.

use std::path::Path;

use crate::error::DiskError;
use crate::nbd;
use crate::nbd::unique_mount_name;

/// Каталог с бинарниками Magisk, указанный пользователем через
/// `--magisk-dir`. Должен содержать как минимум `magisk` и `magiskinit`.
/// Опциональные файлы: `busybox`, `magiskboot`, `util-linux/`.
///
/// Проверяется до начала монтирования, чтобы не оставаться в
/// полузвёрзнутом состоянии при невалидном входе.
const REQUIRED_MAGISK_FILES: &[&str] = &["magisk", "magiskinit"];

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
        return Ok(());
    }

    // Ищем boot.img
    let boot_img_candidates = [
        mount_point.join("boot.img"),
        mount_point.join("boot/boot.img"),
    ];

    let boot_img = match boot_img_candidates.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => return Ok(()),
    };
    // See PLAN.md, item 20d — computed once here since `boot_img` is
    // reused across the unpack/patch/repack calls below, rather than
    // repeating `.to_string_lossy().into_owned()` at each call site.
    let boot_img_str = boot_img.to_string_lossy().into_owned();

    // Same rationale as `mount_dir_base()` above — see PLAN.md, item 20a.
    let patch_dir = andler_core::paths::runtime_dir()
        .join("andler-magisk-patch")
        .join(unique_mount_name());
    andler_core::paths::ensure_private_dir_sync(&patch_dir).map_err(|e| {
        DiskError::NbdSetupFailed(format!("failed to create patch dir: {e}"))
    })?;

    // Unpack
    let output = std::process::Command::new(&magiskboot)
        .args(["unpack", &boot_img_str])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot unpack failed: {e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    // Patch
    let patched_boot = patch_dir.join("patched_boot.img");
    let patched_boot_str = patched_boot.to_string_lossy().into_owned();
    let output = std::process::Command::new(&magiskboot)
        .args([
            "patch",
            &boot_img_str,
            &patched_boot_str,
        ])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot patch failed: {e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    // Repack
    let output = std::process::Command::new(&magiskboot)
        .args([
            "repack",
            &patched_boot_str,
            &boot_img_str,
        ])
        .current_dir(&patch_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| DiskError::NbdSetupFailed(format!("magiskboot repack failed: {e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&patch_dir);
        return Ok(());
    }

    let _ = std::fs::remove_dir_all(&patch_dir);

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
/// Async entry point — see [`provision_magisk_blocking`] for the actual
/// work. This wrapper exists because every step of Magisk provisioning
/// (`qemu-nbd`, partition-appearance polling, `mount`, `magiskboot`) is
/// blocking I/O — subprocess calls and a `std::thread::sleep` poll loop
/// — none of which belongs directly on a tokio worker thread. See
/// PLAN.md, item 21c, "Blocking I/O in provision_magisk": the plan's own
/// proposed fix (swap `std::thread::sleep` for `tokio::time::sleep`
/// inside the poll loop) doesn't actually work as stated, since
/// `wait_for_partitions` and everything it calls are plain sync `fn`s,
/// not `async fn` — there's nothing to `.await` from inside them without
/// converting the whole chain to async, which would then need
/// `tokio::process::Command` throughout instead of `std::process::Command`
/// too. `spawn_blocking` the entire synchronous pipeline as one unit is
/// the actual fix that matches how the rest of this module is written
/// (see the module doc comment on why these subprocess calls are sync in
/// the first place), not a partial one that only silences the `sleep`
/// call while every subprocess invocation still blocks the same way.
pub async fn provision_magisk(overlay_path: &Path, magisk_dir: &Path) -> Result<(), DiskError> {
    let overlay_path = overlay_path.to_path_buf();
    let magisk_dir = magisk_dir.to_path_buf();
    tokio::task::spawn_blocking(move || provision_magisk_blocking(&overlay_path, &magisk_dir))
        .await
        .unwrap_or_else(|join_err| {
            Err(DiskError::NbdSetupFailed(format!(
                "magisk provisioning task panicked: {join_err}"
            )))
        })
}

fn provision_magisk_blocking(overlay_path: &Path, magisk_dir: &Path) -> Result<(), DiskError> {
    validate_magisk_dir(magisk_dir)?;

    if !overlay_path.exists() {
        return Err(DiskError::BackingFileNotFound(overlay_path.to_path_buf()));
    }

    let nbd_guard = nbd::connect_nbd(overlay_path)?;
    let partitions = nbd::wait_for_partitions(nbd_guard.path())?;
    let root_partition = nbd::find_root_partition(&partitions)?;
    let mount_guard = nbd::mount_partition(&root_partition)?;

    copy_magisk_files(mount_guard.path(), magisk_dir)?;

    let _ = patch_boot_image(mount_guard.path(), magisk_dir);

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
