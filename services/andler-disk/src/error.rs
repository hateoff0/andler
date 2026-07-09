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

    /// Каталог Magisk невалиден: не существует, не является каталогом,
    /// или не содержит обязательных файлов (`magisk`, `magiskinit`).
    #[error("invalid magisk-dir: {0}")]
    MagiskDirInvalid(String),

    /// Ошибка при работе с NBD-устройством (qemu-nbd/mount/umount):
    /// модуль ядра не загружен, нет свободного nbd-девайса,
    /// не удалось подключить/отключить образ, смонтировать/отмонтировать
    /// раздел.
    #[error("nbd setup failed: {0}")]
    NbdSetupFailed(String),

    /// `resize` запрошен с размером меньше текущего виртуального размера
    /// диска, но вызывающая сторона не подтвердила это явно
    /// (`allow_shrink = false`). См. PLAN.md, раздел «Disk management»:
    /// `qemu-img resize` не уменьшает qcow2 без `--shrink`, и уменьшение
    /// безопасно только если файловая система внутри гостя была заранее
    /// уменьшена — иначе риск потери данных. Это не просьба передать
    /// флаг библиотеке qemu-img, а явный отказ на уровне ANDLER, пока
    /// пользователь не подтвердит операцию осознанно.
    #[error(
        "shrinking {path} from {current_size_bytes} to {requested_size_bytes} bytes requires \
         explicit confirmation (risk of data loss if the guest filesystem was not shrunk first)"
    )]
    ShrinkRequiresConfirmation {
        path: PathBuf,
        current_size_bytes: u64,
        requested_size_bytes: u64,
    },

    /// `compact` запрошен для диска не в формате qcow2 (например, raw).
    /// Для raw-дисков нет qcow2-метаданных, по которым можно было бы
    /// компактифицировать файл — операция в qcow2-понятии неприменима.
    /// См. PLAN.md, раздел «Disk management»: `disk compact` должен
    /// явно сообщать об этом, не пытаться слепо выполнить `qemu-img
    /// convert` и не падать с непонятной ошибкой `qemu-img`.
    #[error("compact is not applicable to `{format}` disks (only qcow2 has reclaimable metadata): {path}")]
    CompactNotApplicable { path: PathBuf, format: String },

    /// Пакетный менеджер не найден в смонтированной ФС гостя — невозможно
    /// установить/удалить пакет через offline-метод.
    #[error("no supported package manager (apt/dnf/pacman) found in guest filesystem: {mount_point}")]
    PackageManagerNotFound { mount_point: PathBuf },

    /// Запрошенный пакет уже установлен в гостевой ФС.
    #[error("package `{package}` is already installed in guest filesystem")]
    AgentAlreadyInstalled { package: String },

    /// Запрошенный пакет не найден в гостевой ФС (для операции удаления).
    #[error("package `{package}` is not installed in guest filesystem")]
    AgentNotInstalled { package: String },

    /// QEMU Guest Agent недоступен в запущенном инстансе (guest-ping не
    /// ответил или guest-exec не поддерживается).
    #[error("QEMU guest agent is not available in instance {instance_id}")]
    GuestAgentUnavailable { instance_id: String },
}
