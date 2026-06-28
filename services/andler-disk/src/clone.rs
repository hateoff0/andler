//! Клонирование диска Android-инстанса в три разных режима — см.
//! `CloneMode` (`andler-core`) за доменным смыслом каждого варианта,
//! здесь только механика на уровне файлов qcow2.
//!
//! Все три режима принимают на вход диск *конкретного инстанса*
//! (`source_disk_path`, обычно overlay с `backing_file = base_image`),
//! не общий `base_image` напрямую — в отличие от `overlay::create_overlay`,
//! которая создаёт *первый* overlay инстанса от общего базового образа.
//! Это разные операции с разной семантикой backing chain (см. обсуждение
//! инварианта "что общее, что личное" в истории проекта), поэтому они не
//! делят код через одну функцию с флагом-режимом, даже там, где
//! получившийся вызов `qemu-img` совпадает (`linked_clone`).

use std::path::{Path, PathBuf};

use crate::error::DiskError;
use crate::qcow2;

/// Результат любой из трёх операций клонирования — путь к новому файлу
/// диска плюс то, на что (если на что-либо) он ссылается как на
/// `backing_file` — для логирования/диагностики на стороне
/// `andler-daemon`, аналогично `overlay::OverlayDisk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClonedDisk {
    pub disk_path: PathBuf,
    /// `None` для `full_standalone_clone` (результат не имеет
    /// backing_file вообще). `Some(source_disk_path)` для `linked_clone`.
    /// `Some(shared_base_path)` для `shared_base_clone` — путь
    /// унаследован из метаданных скопированного файла, не передаётся
    /// явно вызывающей стороной (см. документацию `shared_base_clone`).
    pub backing_file: Option<PathBuf>,
}

/// Linked clone: новый qcow2 с `backing_file = source_disk_path` —
/// источник становится для клона тем же, чем `base_image` является для
/// обычного Android-overlay (см. модульную документацию за тем, почему
/// это всё же не единая функция с `create_overlay`).
///
/// Дешёво и быстро — `qemu-img create -b` не копирует никаких данных,
/// только создаёт пустой файл с указанием, где искать недостающие блоки.
/// Ценой этого: `source_disk_path` становится зависимостью клона — если
/// файл по этому пути исчезнет (например, `remove_instance(purge=true)`
/// на инстансе-источнике), клон сломается. Не проверяется здесь (этот
/// модуль не знает про `InstanceId`/реестр инстансов) — ответственность
/// `andler-daemon::find_live_clones`, вызываемая перед purge.
///
/// `dest_path` — обычно `<новый_instance_dir>/disk.qcow2`; родительский
/// каталог создаётся автоматически, как и у `create_with_backing_file`.
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

/// Full standalone clone: разворачивает всю backing chain
/// (`source_disk_path` и, если он сам overlay, всё, на что он
/// ссылается — обычно общий `base_image` профиля) в один самостоятельный
/// файл без какого-либо `backing_file`.
///
/// Дорого по месту (полный логический размер диска, не разница) и по
/// времени (копируются все блоки, не только метаданные) — в обмен на
/// полную независимость: результат можно перемещать между хостами или
/// держать после того, как удалены и источник, и общий базовый образ.
///
/// Тонкая обёртка над `qcow2::clone_full` — этот модуль существует не
/// потому, что механика отличается (она не отличается), а чтобы дать
/// этой операции то же доменное имя и тот же возвращаемый тип
/// (`ClonedDisk`), что и двум другим режимам клонирования, и держать все
/// три рядом для единообразия на стороне `andler-daemon::clone_instance`.
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

/// Shared-base clone: копия самого файла `source_disk_path` (обычный
/// `tokio::fs::copy`, не `qemu-img create`/`convert`) — не разворачивает
/// backing chain и не создаёт зависимости от источника.
///
/// Работает только потому, что Android-overlay (то, что обычно передают
/// сюда как `source_disk_path`) уже содержит лишь diff относительно
/// `base_image` — сам файл маленький независимо от логического размера
/// диска. Копия этого файла побайтово:
/// - наследует те же qcow2-метаданные, включая путь к `backing_file`
///   (обычно общий `base_image` профиля, не сам `source_disk_path`) —
///   результат остаётся тонким относительно общего образа;
/// - физически не зависит от `source_disk_path` после копирования —
///   удаление исходного инстанса (даже с purge) не повредит клон, в
///   отличие от `linked_clone`.
///
/// `tokio::fs::copy`, а не `qemu-img convert` — `convert` разворачивает
/// backing chain (это `full_standalone_clone`), здесь нужно прямо
/// противоположное: сохранить ссылку на backing_file как есть, скопировав
/// только сам diff-файл. Не сохраняет sparse-дыры так аккуратно, как
/// `qemu-img convert` (см. документацию `qcow2::clone_full`), но для
/// небольшого overlay-файла это не существенно.
///
/// Не проверяет и не возвращает явно, на что в итоге ссылается
/// результат (`ClonedDisk::backing_file` здесь означает "то же, на что
/// ссылался source" — не перепроверяется отдельным вызовом `qemu-img
/// info`, так как копия побайтовая и backing_file гарантированно совпадёт
/// с тем, что было у источника на момент копирования); вызывающая сторона
/// (`andler-daemon`) уже знает это значение из `InstanceConfig` источника
/// и передаёт его явно как `source_base_image`, а не для повторной
/// проверки.
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

    // Все три функции реального клонирования требуют бинарника
    // `qemu-img` (`linked_clone`/`full_standalone_clone`) или хотя бы
    // существующего файла-источника (`shared_base_clone`) — помечены
    // `#[ignore]`, гоняются в `integration-test` Docker-таргете (см.
    // docker/README.md), как и тесты `qcow2.rs`/`overlay.rs`.

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

        // Удаляем источник целиком (имитация remove --purge инстанса A)
        // — клон должен остаться нетронутым и читаемым, так как копия
        // была побайтовой, не backing-связью.
        tokio::fs::remove_dir_all(&source_instance_dir).await.unwrap();
        assert!(dest_disk.exists());
        assert!(qcow2::virtual_size_bytes(&dest_disk).await.is_ok());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn shared_base_clone_reports_missing_source_as_io_error() {
        // Не требует qemu-img — tokio::fs::copy сам вернёт ENOENT для
        // несуществующего источника до того, как любой внешний процесс
        // был бы вызван.
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
