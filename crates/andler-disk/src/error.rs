//! Ошибки операций `andler-disk`.
//!
//! Отдельный тип от `andler_core::BackendError`: `andler-disk` работает на
//! уровень ниже (вызов `qemu-img`), а `BackendError` — это контракт
//! `HypervisorBackend` из `andler-core`. `andler-daemon` транслирует
//! `DiskError` в `BackendError::Io`/`InvalidConfig` там, где нужно — эта
//! трансляция не часть `andler-disk`, чтобы не тащить сюда зависимость на
//! `andler-core` ради одного enum.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiskError {
    /// Процесс `qemu-img` не удалось запустить (бинарник не найден, нет
    /// прав на exec и т.п.) — отличается от `CommandFailed`, где процесс
    /// запустился, но завершился с ошибкой.
    #[error("failed to spawn qemu-img: {0}")]
    SpawnFailed(std::io::Error),

    /// `qemu-img` запустился, но завершился с ненулевым кодом возврата.
    /// `stderr` сохраняется целиком — сообщения `qemu-img` обычно содержат
    /// конкретную причину (например, "Backing file ... not found").
    #[error("qemu-img exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },

    /// Путь, переданный как базовый образ (`backing_file`) для overlay,
    /// не существует на момент вызова. Проверяется до запуска `qemu-img`,
    /// чтобы дать более понятную ошибку, чем то, что вернул бы сам
    /// `qemu-img` в этом случае.
    #[error("backing file does not exist: {0}")]
    BackingFileNotFound(PathBuf),

    /// Не удалось распарсить вывод `qemu-img info --output=json`.
    #[error("failed to parse qemu-img output: {0}")]
    ParseError(String),

    /// Ошибка файловой системы при работе с путями (создание каталогов
    /// под итоговый файл и т.п.) — отдельно от `SpawnFailed`/`CommandFailed`,
    /// которые относятся именно к процессу `qemu-img`.
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
