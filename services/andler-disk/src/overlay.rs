//! Overlay-диски для Android-инстансов: тонкий, доменно-осмысленный слой
//! поверх `qcow2::create_with_backing_file`.
//!
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §4.4.2: каждый
//! Android-инстанс получает overlay-диск с `backing_file`, указывающим на
//! общий read-only базовый образ. Этот модуль создаёт сам файл на диске —
//! `andler_core::config::DiskConfig::overlay()` лишь описывает, каким
//! overlay должен быть (декларативные данные), не создавая ничего.
//!
//! Provisioning root/Magisk (offline-монтирование overlay и копирование
//! модулей перед первым запуском, см. README этого крейта) — TODO,
//! требует работы с loop-устройствами/доступа внутрь файловой системы
//! образа и заведомо выходит за рамки этого шага; см. `create_overlay`
//! ниже — она создаёт пустой (в смысле "без root-модификаций") overlay,
//! готовый для `RootMode::None`.

use std::path::{Path, PathBuf};

use crate::error::DiskError;
use crate::qcow2;

/// Результат создания overlay-диска — путь к overlay плюс путь к базовому
/// образу, на который он указывает (для логирования/диагностики на стороне
/// `andler-daemon`, не используется этим модулем повторно).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayDisk {
    pub overlay_path: PathBuf,
    pub base_image_path: PathBuf,
}

/// Создаёт overlay-диск для Android-инстанса.
///
/// `instance_dir` — каталог конкретного инстанса (например,
/// `/var/lib/andler/instances/<InstanceId>/`); overlay создаётся как
/// `<instance_dir>/disk.qcow2`. Каталог самого инстанса должен уже
/// существовать или быть создаваемым — `qcow2::create_with_backing_file`
/// создаёт отсутствующие родительские каталоги (см. её документацию), так
/// что отдельного предварительного шага здесь не требуется.
///
/// `base_image_path` — путь к уже скачанному и проверенному (см. §4.4.2
/// плана) базовому образу. Если файла не существует — возвращается
/// `DiskError::BackingFileNotFound` раньше, чем будет создан overlay,
/// чтобы частично созданный инстанс не оставался в противоречивом
/// состоянии.
///
/// `overlay_size_bytes` — логический размер overlay (может отличаться от
/// размера базового образа, см. документацию `qcow2::create_with_backing_file`).
///
/// Если по пути `<instance_dir>/disk.qcow2` уже существует файл — функция
/// не проверяет это сама и полагается на то, что `qemu-img create`
/// откажется перезаписать существующий файл без явного флага; вызывающая
/// сторона (`andler-daemon`) отвечает за то, чтобы не вызывать эту функцию
/// повторно для уже существующего инстанса (например, при Factory Reset
/// сначала удаляя старый overlay — см. README этого крейта про Factory
/// Reset как пересоздание overlay).
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

/// Factory Reset — пересоздаёт overlay с нуля, отбрасывая все изменения,
/// сделанные внутри гостя, не трогая базовый образ.
///
/// Реализовано как удаление существующего overlay-файла и повторный вызов
/// `create_overlay` — а не `qemu-img` команда "очистки" overlay, так как
/// в qcow2 нет операции "откатить overlay к пустому состоянию, сохранив
/// backing_file" дешевле, чем пересоздание файла; для thin-provisioned
/// qcow2 это всё равно быстрая операция (см. ADR 0001).
///
/// Инстанс должен быть остановлен до вызова этой функции — она не
/// проверяет это сама (нет доступа к `HypervisorBackend`/FSM на этом
/// уровне, см. границы ответственности в README крейта); вызывает
/// `andler-daemon`, который и владеет проверкой состояния инстанса.
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

    // Реальное создание overlay-файла требует бинарника `qemu-img` —
    // помечено #[ignore], гоняется в integration-test Docker-таргете
    // (см. docker/README.md), как и тесты qcow2.rs.

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
