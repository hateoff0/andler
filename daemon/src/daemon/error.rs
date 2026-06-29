use std::path::PathBuf;

use andler_core::{BackendError, BackendKind, InstanceId, InstanceState};
use andler_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    /// Инстанс с таким `InstanceId` не зарегистрирован в этом `Daemon`
    /// (не путать с `BackendError::HandleNotFound` — это про конкретный
    /// backend-хэндл; эта ошибка — про то, что демон вообще не знает про
    /// такой `InstanceId`).
    #[error("instance {0:?} not found")]
    InstanceNotFound(InstanceId),

    /// Запрошен `BackendKind`, для которого в реестре нет реализации
    /// (на данный момент в реестре по умолчанию есть только `Qemu` —
    /// см. `Daemon::new`).
    #[error("no backend registered for {0:?}")]
    NoBackendRegistered(BackendKind),

    /// Переход FSM не разрешён из текущего состояния (см.
    /// `andler_core::fsm`) — например, `pause` для инстанса, который
    /// ещё не был запущен.
    #[error("invalid state transition: {0}")]
    InvalidTransition(#[from] andler_core::FsmError),

    /// Backend вернул ошибку при выполнении операции.
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),

    /// Ошибка `andler-disk` при создании overlay-диска для Android-инстанса
    /// (см. `create_android_instance`).
    #[error("disk error: {0}")]
    Disk(#[from] andler_disk::DiskError),

    /// Ошибка файловой системы вне `andler-disk` — на данный момент это
    /// только подготовка персональной копии `OVMF_VARS` для нового
    /// Android-инстанса (создание каталога инстанса, копирование шаблона).
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Ошибка `andler-store` при восстановлении инстансов из базы при
    /// старте (`Daemon::restore`). Не возникает из обычных операций
    /// (`create_instance`/переходы FSM) — там сбой записи в store
    /// логируется и не прерывает операцию, см. комментарий у
    /// `persist_state`/`persist_new_instance`; здесь же, на старте,
    /// демон ещё не отдаёт никаких гарантий клиентам, и нет смысла
    /// поднимать процесс с заведомо нечитаемой базой.
    #[error("failed to restore instances from store: {0}")]
    Restore(#[from] StoreError),

    /// `remove_instance` вызван для записи в нетерминальном состоянии
    /// (`Starting`/`Running`/`Paused`/`Stopping`) — см. документацию
    /// `Daemon::remove_instance` за тем, почему удаление запущенного
    /// инстанса запрещено, а не просто молча останавливает его сначала.
    #[error("cannot remove instance {0:?}: it is in non-terminal state {1:?}; stop it first")]
    InstanceNotRemovable(InstanceId, InstanceState),

    /// `clone_instance`/`export_instance_disk` вызван для записи в
    /// нетерминальном состоянии — копировать/линковать диск живого
    /// процесса QEMU небезопасно без stop или отдельного live-snapshot
    /// механизма QEMU (которого здесь нет), см. документацию
    /// `Daemon::clone_instance`.
    #[error(
        "cannot clone/export instance {0:?}: it is in non-terminal state {1:?}; stop it first"
    )]
    InstanceNotClonable(InstanceId, InstanceState),

    /// `clone_instance` вызван с `CloneMode::SharedBase` для
    /// `InstanceKind::LinuxVm` — `SharedBase` предполагает общий
    /// `base_image` профиля, которого у `LinuxVm` нет (диск standalone).
    /// См. документацию `Daemon::clone_instance`.
    #[error("shared-base clone is not supported for LinuxVm: {0:?}")]
    SharedBaseNotSupportedForLinuxVm(InstanceId),

    /// `remove_instance(purge: true)` вызван для инстанса, у которого
    /// есть один или больше `CloneMode::Linked`-клонов, всё ещё
    /// ссылающихся на его диск как на `backing_file` — purge удалил бы
    /// файл, от которого зависят эти клоны, оставив их сломанными. См.
    /// документацию `Daemon::find_live_clones`.
    #[error(
        "cannot purge instance {0:?}: it has live linked clones {1:?}; remove them first"
    )]
    InstanceHasLiveClones(InstanceId, Vec<InstanceId>),

    /// Снапшот с таким тегом не найден для данного инстанса.
    #[error("snapshot {tag:?} not found for instance {instance_id:?}")]
    SnapshotNotFound {
        instance_id: InstanceId,
        tag: String,
    },

    /// Снапшот с таким тегом уже существует для данного инстанса.
    #[error("snapshot {tag:?} already exists for instance {instance_id:?}")]
    SnapshotAlreadyExists {
        instance_id: InstanceId,
        tag: String,
    },

    /// Операция над снапшотом требует запущенный инстанс (`Running`/`Paused`),
    /// потому что выполняется через QMP (snapshot-save/load/delete).
    #[error("snapshot operation requires running instance {0:?}, but state is {1:?}")]
    SnapshotOperationRequiresRunningInstance(InstanceId, InstanceState),
}
