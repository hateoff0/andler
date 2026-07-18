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

    /// Ошибка `andler-firmware` при детекции OVMF или провизионировании
    /// персональной копии `OVMF_VARS`. Например, OVMF/EDK2 не установлен
    /// в системе, или не удалось скопировать шаблон VARS в каталог инстанса.
    #[error("firmware error: {0}")]
    Firmware(String),

    /// Ошибка файловой системы вне `andler-disk` — создание каталога
    /// инстанса и прочие FS-операции, не связанные с OVMF.
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

    /// Инстанс уже достиг `MAX_SNAPSHOTS_PER_INSTANCE` (см.
    /// `snapshot_ops.rs`) — новый снапшот не создаётся, пока пользователь
    /// не удалит хотя бы один существующий. Без этого лимита internal
    /// snapshots (каждый хранится прямо внутри qcow2-файла) могут расти
    /// неограниченно, постепенно раздувая сам файл диска и удлиняя время
    /// snapshot-save/-load — лимит даёт понятную, конкретную ошибку
    /// вместо постепенной деградации, которую пользователь не связал бы
    /// с количеством снапшотов.
    #[error(
        "instance {instance_id:?} already has {current} snapshots (limit {limit}); delete one before creating another"
    )]
    SnapshotLimitExceeded {
        instance_id: InstanceId,
        current: usize,
        limit: usize,
    },

    /// Пользователь передал пустую строку как ссылку на инстанс (ни
    /// полный UUID, ни префикс). Отдельно от `InvalidInstanceRef` ниже,
    /// чтобы сообщение было конкретным, а не "prefix '' matches 0
    /// instances".
    #[error("instance reference must not be empty")]
    EmptyInstanceRef,

    /// `Daemon::update_instance_config` (`andler edit`, см. PLAN.md,
    /// "18. Instance config editing") — присланная конфигурация ссылается
    /// на другой `InstanceId`, чем тот, что был разрешён из запроса.
    /// Явная ошибка, а не тихая перезапись под правильным id или запись
    /// под чужим ключом — оба варианта хуже, чем сказать пользователю,
    /// что именно не так.
    #[error("cannot update instance {expected:?}: config has a different id {actual:?}")]
    ConfigIdMismatch {
        expected: InstanceId,
        actual: InstanceId,
    },

    /// `update_instance_config` — присланная конфигурация меняет тип
    /// гостя (`LinuxVm` <-> `AndroidVm`). Не поддерживается: у двух типов
    /// принципиально разный резолв диска/firmware (`AndroidProfile::resolve`
    /// vs прямой `DiskConfig`), смена на лету потребовала бы повторного
    /// провижининга, а не просто замены полей структуры.
    #[error(
        "cannot change instance {0:?} kind (LinuxVm <-> AndroidVm) via edit; \
         recreate the instance instead"
    )]
    ConfigKindChanged(InstanceId),

    /// `update_instance_config` — присланная конфигурация меняет
    /// `disk.path`. Путь к диску должен продолжать указывать на реальный
    /// файл на диске — изменения пути должны идти через `andler disk`/
    /// clone-команды, которые знают, как безопасно переместить/отследить
    /// сам файл, а не через свободное редактирование этого поля.
    #[error(
        "cannot change instance {0:?} disk path via edit; use `andler disk` commands instead"
    )]
    ConfigDiskPathChanged(InstanceId),

    /// `Daemon::resolve_instance_id` — строка не является ни валидным UUID,
    /// ни валидным hex-префиксом (содержит символы вне [0-9a-fA-F-]).
    /// Отдельно от `InstanceRefNotFound`, потому что там ошибка про
    /// отсутствие совпадений, а здесь — про невалидный формат ввода.
    #[error("instance reference {0:?} is not a valid UUID or hex prefix")]
    MalformedInstanceRef(String),

    /// `Daemon::resolve_instance_id` — префикс (см. "Partial instance ID"
    /// в PLAN.md, по аналогии с Docker) не совпал ни с одним
    /// зарегистрированным `InstanceId`. Отдельно от `InstanceNotFound`,
    /// потому что там ошибка про заведомо корректный, но незарегистрированный
    /// `InstanceId`, а здесь — про сам пользовательский ввод (нет ни
    /// одного кандидата вообще, включая случай опечатки).
    #[error("no instance found matching {0:?}")]
    InstanceRefNotFound(String),

    /// `Daemon::resolve_instance_id` — префикс совпал больше чем с одним
    /// зарегистрированным `InstanceId`. Список кандидатов идёт в
    /// сообщение целиком (их обычно не больше единиц/десятков), чтобы
    /// пользователь сразу увидел, чем их различить, не гадая и не делая
    /// повторный `andler status` за полным списком.
    #[error("instance reference {prefix:?} is ambiguous, matches: {candidates:?}")]
    AmbiguousInstanceId {
        prefix: String,
        candidates: Vec<InstanceId>,
    },

    /// QEMU Guest Agent недоступен для инстанса (guest-ping не ответил
    /// или guest-exec не поддерживается). Используется когда VM запущена,
    /// но online-установка невозможна — пользователю предлагается
    /// остановить VM для offline-установки через qemu-nbd.
    #[error("guest agent unavailable for instance {instance_id:?}: {message}")]
    GuestAgentUnavailable {
        instance_id: InstanceId,
        message: String,
    },

    /// `set_instance_config` — ключ конфигурации не является валидным
    /// ключом (например, "arm_translator", "name", "root") или значение
    /// не может быть распарсено.
    #[error("invalid config key: {0:?}")]
    InvalidConfigKey(String),

    /// `switch_arm_translator` — инстанс не является AndroidVm.
    #[error("instance {0:?} is not an Android VM")]
    NotAndroid(InstanceId),

    /// `set_instance_config`/`switch_arm_translator` — инстанс должен
    /// быть в нетерминальном состоянии (Stopped) для изменений,
    /// затрагивающих overlay-диск.
    #[error("instance {0:?} must be stopped (currently {1:?}) to change config")]
    InstanceMustBeStopped(InstanceId, InstanceState),

}
