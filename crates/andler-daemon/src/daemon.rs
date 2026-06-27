//! `Daemon` — связывает реестр `HypervisorBackend`-реализаций
//! (`andler-qemu::QemuBackend`, в перспективе `andler-vmm`) с FSM из
//! `andler-core` и временным in-memory хранением `InstanceConfig`.
//!
//! Это ядро `andlerd` без сети и без персистентности: `andler-rpc`/gRPC и
//! `andler-store`/SQLite — следующие слои поверх этой структуры, не часть
//! неё. `Daemon` отвечает на вопрос "что происходит, когда нужно создать,
//! запустить, поставить на паузу или остановить инстанс", а не "как до
//! этого добраться по сети" или "как это переживёт перезапуск процесса".
//!
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §5 (API/RPC) — методы
//! здесь соответствуют будущим gRPC-методам по смыслу один-к-одному, но
//! сами пока не знают про gRPC.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use andler_core::{
    AndroidProfile, BackendError, BackendHandle, BackendKind, BackendStatus, CloneMode,
    HypervisorBackend, InstanceConfig, InstanceEvent, InstanceId, InstanceKind, InstanceState,
    LogLine,
};
use andler_qemu::QemuBackend;
use andler_store::{Store, StoreError};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::RwLock;

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
        path: std::path::PathBuf,
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

    /// `clone_instance`/`export_instance_disk` вызван для
    /// `InstanceKind::LinuxVm` — за пределами текущего шага реализации,
    /// см. документацию `Daemon::clone_instance`.
    #[error("clone/export is only supported for AndroidVm instances, not {0:?}")]
    CloneNotSupportedForKind(InstanceId),

    /// `remove_instance(purge: true)` вызван для инстанса, у которого
    /// есть один или больше `CloneMode::Linked`-клонов, всё ещё
    /// ссылающихся на его диск как на `backing_file` — purge удалил бы
    /// файл, от которого зависят эти клоны, оставив их сломанными. См.
    /// документацию `Daemon::find_live_clones`.
    #[error(
        "cannot purge instance {0:?}: it has live linked clones {1:?}; remove them first"
    )]
    InstanceHasLiveClones(InstanceId, Vec<InstanceId>),
}

/// Реестр backend'ов по умолчанию: `Qemu -> QemuBackend`. `Vmm`
/// сознательно не регистрируется — `andler-vmm` пустой каркас (см. его
/// README). Вынесена в свободную функцию, а не инлайнится в каждый
/// конструктор `Daemon` (`new`/`with_store`/`restore`), чтобы реестр по
/// умолчанию не мог разойтись между ними.
fn default_backends() -> HashMap<BackendKind, Arc<dyn HypervisorBackend>> {
    let mut backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>> = HashMap::new();
    backends.insert(BackendKind::Qemu, Arc::new(QemuBackend::new()));
    backends
}

/// Внутреннее состояние одного инстанса с точки зрения `Daemon`:
/// конфигурация (нужна, чтобы повторно резолвить `spawn`, и для будущего
/// сохранения в `andler-store`), текущее состояние FSM, и backend-хэндл —
/// есть только после первого успешного `start_instance` (до этого инстанс
/// существует как конфигурация, но ничего не запущено).
struct InstanceRecord {
    config: InstanceConfig,
    state: InstanceState,
    handle: Option<BackendHandle>,
}

/// Ядро `andlerd`: реестр backend'ов + текущее in-memory состояние
/// инстансов + опциональная персистентность.
///
/// `instances` — под `tokio::sync::RwLock`, не `Mutex`: `status()`
/// (предполагаемо частый вызов, например при опросе из GUI) — это только
/// чтение записи демона о состоянии плюс один вызов backend'а; не имеет
/// смысла блокировать другие конкурентные чтения статусов на время одного
/// такого вызова так же, как блокировались бы записи.
///
/// `store: Option<Store>`, а не безусловный `Store` — `Daemon::new()`
/// (без персистентности) остаётся валидным способом получить `Daemon`,
/// используемым во всех существующих тестах `daemon::tests` и в
/// `grpc_roundtrip_test`: они проверяют поведение FSM/backend'а, и
/// обязывать их таскать sqlite (даже in-memory) было бы лишней связностью
/// без какой-либо пользы для того, что эти тесты проверяют. `None` — это
/// не временное упущение, а осознанный режим "только in-memory",
/// симметричный тому, что было до появления `andler-store` вообще.
pub struct Daemon {
    backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    instances: RwLock<HashMap<InstanceId, InstanceRecord>>,
    store: Option<Store>,
}

/// RAII-страховка от мусора на диске при частично неудавшемся
/// `create_android_instance`: удаляет `instance_dir` целиком при выходе
/// из скоупа, если не был явно разряжён (`disarm()`) после того, как вся
/// последовательность (создание каталога → копирование `OVMF_VARS` →
/// overlay-диск → регистрация в `Daemon::create_instance`) завершилась
/// успешно.
///
/// RAII, а не `tokio::fs::remove_dir_all` в каждой ветке `?` по отдельности
/// — расставлять очистку вручную на каждой точке выхода надёжно ровно до
/// тех пор, пока кто-то не добавит новую точку сбоя в середину метода и
/// не забудет про неё; guard убирает за собой при *любом* раннем
/// возврате, включая будущие, которые сегодня ещё не написаны.
///
/// `Drop::drop` синхронный, поэтому очистка — `std::fs::remove_dir_all`,
/// не `tokio::fs::remove_dir_all`: `Drop` не может быть `async`, а
/// блокировать executor на удаление нескольких файлов в каталоге одного
/// инстанса — приемлемо, поскольку этот путь срабатывает только при
/// ошибке создания (не на каждый успешный вызов) и удаляет малый,
/// заранее известный набор файлов (VARS.fd, overlay/base qcow2), не
/// произвольно большое дерево.
struct InstanceDirGuard {
    path: PathBuf,
    armed: bool,
}

impl InstanceDirGuard {
    fn new(path: PathBuf) -> Self {
        InstanceDirGuard { path, armed: true }
    }

    /// Снимает страховку — вызывается после того, как `instance_dir`
    /// больше не нужно удалять (вся последовательность создания
    /// инстанса завершилась успешно, включая регистрацию в `Daemon`).
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InstanceDirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Ошибка самой очистки (например, гонка с параллельным удалением)
        // намеренно проглатывается, не паникует и не логируется через
        // `tracing` — `Drop` это не место для эскалации новой ошибки на
        // фоне уже идущей (метод, вызывающий этот guard, уже возвращает
        // `Err` по другой причине); путь к каталогу виден через
        // `self.path`, если потребуется диагностика вручную.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Удаляет файлы инстанса, принадлежащие только ему (диск, персональная
/// копия OVMF_VARS), и делает best-effort попытку убрать опустевший
/// родительский каталог. Используется только из `Daemon::remove_instance`
/// при `purge: true` — см. документацию там за полным обоснованием того,
/// что удаляется и почему.
///
/// Каждая ошибка удаления логируется отдельно, не агрегируется и не
/// возвращается — вызывающая сторона (`remove_instance`) уже считает
/// purge best-effort шагом после того, как сама запись инстанса уже
/// удалена; частичный сбой здесь не должен превращаться в `Err` для уже
/// выполненной операции удаления.
async fn purge_instance_files(id: InstanceId, config: &InstanceConfig) {
    if let Err(err) = tokio::fs::remove_file(&config.disk.path).await {
        tracing::error!(
            instance_id = %id.0,
            path = %config.disk.path.display(),
            error = %err,
            "purge: failed to remove instance disk file"
        );
    }

    if let Err(err) = tokio::fs::remove_file(&config.firmware.ovmf_vars_path).await {
        tracing::error!(
            instance_id = %id.0,
            path = %config.firmware.ovmf_vars_path.display(),
            error = %err,
            "purge: failed to remove instance OVMF_VARS file"
        );
    }

    // Best-effort: только убирает каталог, если он уже пуст (т.е. оба
    // файла выше были единственным его содержимым — типичный случай для
    // `AndroidVm::instance_dir`). Непустой каталог (типичный случай для
    // `LinuxVm` с пользовательским путём, где рядом могут быть чужие
    // файлы) тихо остаётся на месте — это не ошибка purge, а ожидаемый
    // исход для путей вне `instance_dir`.
    if let Some(parent) = config.disk.path.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
}

impl Daemon {
    /// Создаёт `Daemon` с реестром backend'ов по умолчанию: `Qemu` ->
    /// `QemuBackend`. `Vmm` сознательно не регистрируется здесь —
    /// `andler-vmm` пустой каркас (см. его README), регистрация
    /// несуществующей полноценной реализации не добавила бы ценности;
    /// когда `andler-vmm` будет готов к регистрации, эта точка —
    /// естественное место, где это сделать.
    ///
    /// Без персистентности (`store: None`) — см. документацию поля
    /// `Daemon::store`. Для персистентности используй `with_store`/
    /// `restore`.
    pub fn new() -> Self {
        Self::with_backends_and_store(default_backends(), None)
    }

    /// Как `Daemon::new`, но с персистентностью: каждая успешная
    /// `create_instance` и каждый успешный переход FSM в
    /// `start_instance`/`stop_instance` сохраняются в `store`. Не
    /// восстанавливает уже существующие в `store` записи — для этого
    /// нужен `Daemon::restore`. Этот конструктор существует отдельно от
    /// `restore`, потому что не любой вызывающий хочет восстановление при
    /// каждом создании `Daemon` (например, тест, который хочет чистый
    /// `Daemon` с реальной персистентностью, но без шума от уже
    /// существующих записей).
    pub fn with_store(store: Store) -> Self {
        Self::with_backends_and_store(default_backends(), Some(store))
    }

    /// Восстанавливает `Daemon` из ранее сохранённых в `store` инстансов
    /// — основной путь для `main.rs` при старте `andlerd`.
    ///
    /// Backend-хэндлы (`HypervisorBackend`-специфичные дескрипторы
    /// запущенного процесса) никогда не сохраняются в `store` и не могут
    /// быть восстановлены: реальный процесс QEMU (если он был) либо уже
    /// завершился вместе с предыдущим `andlerd`, либо продолжает жить
    /// как осиротевший процесс, о котором этот новый `Daemon` ничего не
    /// знает и с которым не может взаимодействовать (QMP-сокет был
    /// открыт прошлым процессом, его файлового пути недостаточно для
    /// "переподключения" в текущей реализации `andler-qemu`).
    ///
    /// Поэтому любая запись, восстановленная в нетерминальном состоянии
    /// (`Starting`/`Running`/`Paused`/`Stopping` — то есть в состоянии,
    /// которое подразумевает существование живого backend-хэндла),
    /// принудительно переводится в `InstanceState::Error` с понятным
    /// сообщением, а не восстанавливается как есть. Восстановление
    /// `Running` как `Running` было бы ложью: у записи нет `handle`
    /// (он не персистентен), значит `pause_instance`/`stop_instance`
    /// немедленно вернули бы `BackendError::HandleNotFound` — то есть
    /// демон утверждал бы дважды противоречащее: "инстанс работает", но
    /// "у меня нет к нему доступа". Явный `Error` сразу сообщает
    /// пользователю реальное положение дел: после перезапуска `andlerd`
    /// инстанс нужно явно пересоздать/перезапустить, и не оставляет
    /// записи в состоянии, из которого FSM не запрещает (но фактически
    /// не может выполнить) переходы.
    ///
    /// `InstanceState::Created`/`Stopped`/`Error { .. }` восстанавливаются
    /// как есть — это терминальные либо предзапусковые состояния, для
    /// которых отсутствие хэндла является штатным, а не противоречием.
    pub async fn restore(store: Store) -> Result<Self, DaemonError> {
        let stored = store.load_all().await?;

        let mut instances = HashMap::with_capacity(stored.len());
        for entry in stored {
            let id = entry.config.id;
            let state = match entry.state {
                state @ (InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }) => {
                    state
                }
                lost_state @ (InstanceState::Starting
                | InstanceState::Running
                | InstanceState::Paused
                | InstanceState::Stopping) => {
                    tracing::warn!(
                        instance_id = %id.0,
                        previous_state = ?lost_state,
                        "restored instance was not in a terminal state before restart; \
                         backend handle cannot be recovered, marking as Error"
                    );
                    InstanceState::Error {
                        message: format!(
                            "andlerd restarted while instance was in state {lost_state:?}; \
                             backend handle was not persisted and cannot be recovered, \
                             instance must be restarted explicitly"
                        ),
                    }
                }
            };

            instances.insert(
                id,
                InstanceRecord {
                    config: entry.config,
                    state,
                    handle: None,
                },
            );
        }

        Ok(Self::with_backends_and_store_and_instances(
            default_backends(),
            Some(store),
            instances,
        ))
    }

    fn with_backends_and_store(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
    ) -> Self {
        Self::with_backends_and_store_and_instances(backends, store, HashMap::new())
    }

    fn with_backends_and_store_and_instances(
        backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
        store: Option<Store>,
        instances: HashMap<InstanceId, InstanceRecord>,
    ) -> Self {
        Daemon {
            backends,
            instances: RwLock::new(instances),
            store,
        }
    }

    fn backend_for(&self, kind: BackendKind) -> Result<&Arc<dyn HypervisorBackend>, DaemonError> {
        self.backends
            .get(&kind)
            .ok_or(DaemonError::NoBackendRegistered(kind))
    }

    /// Сохраняет новую запись в `store`, если персистентность включена.
    /// Ошибка записи в store **логируется, но не возвращается** вызывающей
    /// стороне `create_instance` — in-memory `instances` уже обновлён к
    /// моменту вызова этого метода, и единственная альтернатива "вернуть
    /// ошибку" — откатить уже сделанную in-memory вставку, что усложняет
    /// код ради сценария (диск недоступен/полон), в котором сам процесс
    /// `andlerd`, скорее всего, и так не протянет долго. Текущая сессия
    /// демона продолжает корректно работать с in-memory данными; разойдётся
    /// только переживание перезапуска — деградация персистентности, не
    /// отказ операции, которую попросил клиент.
    async fn persist_new_instance(&self, cfg: &InstanceConfig, state: &InstanceState) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.save_instance(cfg, state).await {
            tracing::error!(
                instance_id = %cfg.id.0,
                error = %err,
                "failed to persist new instance to store"
            );
        }
    }

    /// Сохраняет обновлённое состояние FSM существующей записи в `store`,
    /// если персистентность включена. См. документацию
    /// `persist_new_instance` про то, почему ошибка только логируется.
    async fn persist_state(&self, id: InstanceId, state: &InstanceState) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.save_state(id, state).await {
            tracing::error!(
                instance_id = %id.0,
                error = %err,
                "failed to persist instance state transition to store"
            );
        }
    }

    /// Регистрирует новый инстанс с состоянием `Created`. Ничего не
    /// запускает — соответствует `andler_core::fsm::InstanceState::Created`:
    /// конфигурация принята и сохранена, процесс ещё не существует.
    /// Backend для `cfg.backend` должен быть зарегистрирован — проверяется
    /// здесь же, до сохранения записи, чтобы не создавать инстанс, который
    /// заведомо невозможно запустить.
    pub async fn create_instance(&self, cfg: InstanceConfig) -> Result<InstanceId, DaemonError> {
        self.backend_for(cfg.backend)?;

        let id = cfg.id;
        {
            let mut instances = self.instances.write().await;
            instances.insert(
                id,
                InstanceRecord {
                    config: cfg.clone(),
                    state: InstanceState::Created,
                    handle: None,
                },
            );
        }
        self.persist_new_instance(&cfg, &InstanceState::Created).await;

        Ok(id)
    }

    /// Резолвит `AndroidProfile` в полноценный инстанс и регистрирует его —
    /// связывает `AndroidProfile::resolve()` (домен, `andler-core`) с
    /// реальным созданием overlay-диска на диске (`andler-disk::overlay`)
    /// и персональной копии `OVMF_VARS`, и только потом передаёт получившийся
    /// `InstanceConfig` в `create_instance` (то есть проходит ту же
    /// валидацию backend'а и попадает в тот же реестр, что и инстанс,
    /// созданный напрямую из готового `InstanceConfig`).
    ///
    /// `instances_root` — каталог, под которым у каждого инстанса свой
    /// подкаталог `<instances_root>/<InstanceId>/` (overlay-диск и
    /// `VARS.fd` внутри него) — соответствует `/var/lib/andler/instances/`
    /// из архитектурного плана, но путь не хардкодится здесь, чтобы тесты
    /// могли передать временный каталог.
    ///
    /// `base_image_path` — путь к уже скачанному и проверенному базовому
    /// образу для этого профиля; получение этого пути (кэш/скачивание) —
    /// явно вне рамок этого метода (см. документацию `AndroidProfile::resolve`)
    /// и пока не реализовано ни здесь, ни где-либо ещё в системе.
    ///
    /// `ovmf_vars_template` — путь к системному шаблону `OVMF_VARS`
    /// (например, `/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd`), который
    /// копируется в персональную копию инстанса, а не используется
    /// напрямую — каждый инстанс должен иметь свою копию, так как UEFI
    /// пишет в этот файл во время работы (boot order, Secure Boot keys и
    /// т.п.), и общий файл между инстансами привёл бы к гонкам/порче
    /// состояния друг друга.
    ///
    /// Если создание каталога инстанса, копирование шаблона `OVMF_VARS`
    /// или создание overlay-диска завершается ошибкой — инстанс не
    /// регистрируется в `Daemon` вообще (никакой частично созданной
    /// записи), и любые уже созданные на диске файлы (каталог,
    /// скопированный `VARS.fd`, overlay-диск, если до них дошло)
    /// удаляются через `InstanceDirGuard` — см. его документацию.
    /// Очистка срабатывает при ЛЮБОМ раннем возврате этого метода,
    /// включая отказ финального `self.create_instance(cfg)` (например,
    /// `NoBackendRegistered`) — на этот момент уже создан весь
    /// `instance_dir` с overlay-диском, и оставлять его на диске без
    /// зарегистрированной записи было бы такой же тихой утечкой, как и
    /// при более ранних точках сбоя.
    pub async fn create_android_instance(
        &self,
        profile: AndroidProfile,
        instance_name: String,
        base_image_path: PathBuf,
        instances_root: PathBuf,
        overlay_size_bytes: u64,
        ovmf_vars_template: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        let id = InstanceId::new();
        let instance_dir = instances_root.join(id.0.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());

        tokio::fs::create_dir_all(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        let ovmf_vars_path = instance_dir.join("VARS.fd");
        tokio::fs::copy(&ovmf_vars_template, &ovmf_vars_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: ovmf_vars_path.clone(),
                source,
            })?;

        let overlay = andler_disk::overlay::create_overlay(
            &instance_dir,
            &base_image_path,
            overlay_size_bytes,
        )
        .await?;

        let mut cfg = profile.resolve(
            instance_name,
            overlay.base_image_path,
            overlay.overlay_path,
            overlay_size_bytes,
            ovmf_vars_path,
        );
        // `resolve()` сама генерирует InstanceId (она ничего не знает про
        // instance_dir/overlay, которые мы уже создали под заранее выбранным
        // `id`) — перезаписываем тем `id`, под которым реально лежат файлы
        // на диске, иначе InstanceConfig.id разойдётся с именем каталога.
        cfg.id = id;

        let registered_id = self.create_instance(cfg).await?;
        // Вся последовательность (каталог, OVMF_VARS, overlay,
        // регистрация в Daemon) успешна — instance_dir больше не
        // подлежит автоматической очистке через Drop этого guard'а.
        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Клонирует существующий Android-инстанс в новый, независимый
    /// `InstanceId` — три режима диска (`CloneMode`, см. его документацию
    /// за полным обоснованием), общая для всех трёх логика вокруг этого:
    /// проверка состояния источника, копирование `OVMF_VARS`, сборка
    /// нового `InstanceConfig`, регистрация через `create_instance`.
    ///
    /// Только `InstanceKind::AndroidVm` — `LinuxVm` не имеет управляемого
    /// `instances_root`, куда можно было бы детерминированно положить
    /// файлы клона (пользовательский путь, переданный через `--file` при
    /// создании оригинала, не подразумевает никакого "соседнего" места
    /// для клона) — попытка клонировать `LinuxVm` возвращает
    /// `DaemonError::CloneNotSupportedForKind`, см. обсуждение в истории
    /// проекта за тем, почему это явный отказ, не молчаливая выдумка
    /// пути.
    ///
    /// Источник должен быть в терминальном состоянии
    /// (`Created`/`Stopped`/`Error`) — копировать/линковать диск живого
    /// процесса QEMU небезопасно (см. `DaemonError::InstanceNotClonable`),
    /// по той же причине, что и у `remove_instance`.
    ///
    /// Клонирование уже существующего клона разрешено и не требует
    /// особого случая здесь: с точки зрения этого метода исходный
    /// инстанс — это просто запись с `InstanceConfig`, откуда взять
    /// `disk.path`/`firmware.ovmf_vars_path`; то, что эта запись сама
    /// была создана как `Linked`/`SharedBase`-клон чего-то ещё, не имеет
    /// значения для построения нового клона от неё.
    ///
    /// `instances_root` — тот же смысл, что и у `create_android_instance`
    /// (не хардкодится, чтобы тесты могли передать временный каталог).
    ///
    /// При сбое посередине (после создания каталога клона, но до
    /// успешной регистрации) — `InstanceDirGuard` убирает частично
    /// созданный `instance_dir`, как и в `create_android_instance`.
    pub async fn clone_instance(
        &self,
        source_id: InstanceId,
        new_name: String,
        instances_root: PathBuf,
        mode: CloneMode,
    ) -> Result<InstanceId, DaemonError> {
        let source_config = self.terminal_android_instance_config(source_id).await?;

        let new_id = InstanceId::new();
        let instance_dir = instances_root.join(new_id.0.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());

        tokio::fs::create_dir_all(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        let new_ovmf_vars_path = instance_dir.join("VARS.fd");
        tokio::fs::copy(&source_config.firmware.ovmf_vars_path, &new_ovmf_vars_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: new_ovmf_vars_path.clone(),
                source,
            })?;

        let new_disk_path = instance_dir.join("disk.qcow2");
        let cloned_disk = match mode {
            CloneMode::Linked => {
                andler_disk::clone::linked_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    source_config.disk.size_bytes,
                )
                .await?
            }
            CloneMode::FullStandalone => {
                andler_disk::clone::full_standalone_clone(&source_config.disk.path, &new_disk_path)
                    .await?
            }
            CloneMode::SharedBase => {
                // `SharedBase` требует общий `base_image` — для
                // Android-инстанса он есть всегда (см. `DiskConfig::overlay`,
                // используемую `create_android_instance`), но тип
                // `DiskConfig.base_image: Option<PathBuf>` этого не
                // гарантирует статически. Отсутствие `base_image` здесь
                // означало бы, что источник сам не overlay (не должно
                // происходить для записи, прошедшей
                // `terminal_android_instance_config`, — `AndroidVm`
                // всегда создаётся с overlay через `create_android_instance`
                // — но явная ошибка лучше `unwrap`, если это
                // предположение когда-нибудь нарушится).
                let base_image = source_config.disk.base_image.clone().ok_or_else(|| {
                    DaemonError::Io {
                        path: source_config.disk.path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "SharedBase clone requires a source disk with base_image set",
                        ),
                    }
                })?;
                andler_disk::clone::shared_base_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    &base_image,
                )
                .await?
            }
        };

        let mut new_config = source_config;
        new_config.id = new_id;
        new_config.name = new_name;
        new_config.disk.path = cloned_disk.disk_path;
        new_config.disk.base_image = cloned_disk.backing_file;
        new_config.firmware.ovmf_vars_path = new_ovmf_vars_path;

        let registered_id = self.create_instance(new_config).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Экспортирует диск Android-инстанса как один самостоятельный файл
    /// по указанному пути — для переноса между хостами или бэкапа, не
    /// для создания нового управляемого инстанса (в отличие от
    /// `clone_instance` с `CloneMode::FullStandalone`, этот метод не
    /// регистрирует ничего в `Daemon`: результат — файл, о котором
    /// `Daemon` больше ничего не знает и не отвечает за него).
    ///
    /// Реализовано через тот же `andler_disk::clone::full_standalone_clone`
    /// (разворачивает всю backing chain, включая общий `base_image`, в
    /// один файл) — переиспользует механику `CloneMode::FullStandalone`
    /// без дублирования кода, просто без шагов "создать каталог
    /// инстанса"/"скопировать OVMF_VARS"/"зарегистрировать", которые
    /// специфичны для создания нового инстанса, не для экспорта файла.
    ///
    /// Источник должен быть в терминальном состоянии — та же причина, что
    /// у `clone_instance`. Только `InstanceKind::AndroidVm` — та же
    /// причина, что у `clone_instance` (нет принципиальной проблемы
    /// экспортировать диск `LinuxVm`, но раз весь `LinuxVm`-путь для
    /// клонирования отложен, последовательность ради единообразия важнее
    /// гипотетического удобства, которое сейчас никто не просил).
    ///
    /// Не использует `InstanceDirGuard` — `dest_path` не "каталог
    /// инстанса", это один файл по пути, который выбрал и за который
    /// отвечает вызывающий; при сбое самого `full_standalone_clone` не
    /// остаётся частично созданного состояния, требующего откат (либо
    /// файл не создан вообще, либо `qemu-img convert` не оставляет
    /// частично записанный файл по контракту самого `qemu-img`).
    pub async fn export_instance_disk(
        &self,
        source_id: InstanceId,
        dest_path: PathBuf,
    ) -> Result<(), DaemonError> {
        let source_config = self.terminal_android_instance_config(source_id).await?;

        andler_disk::clone::full_standalone_clone(&source_config.disk.path, &dest_path).await?;

        Ok(())
    }

    /// Общая проверка для `clone_instance`/`export_instance_disk`:
    /// инстанс существует, в терминальном состоянии, и это `AndroidVm` —
    /// см. документацию обоих методов за тем, почему именно эти условия.
    /// Возвращает клон `InstanceConfig` источника (не ссылку — read-lock
    /// `instances` отпускается до того, как вызывающая сторона начинает
    /// файловые операции, которые могут занять заметное время для
    /// `FullStandalone`).
    async fn terminal_android_instance_config(
        &self,
        id: InstanceId,
    ) -> Result<InstanceConfig, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances.get(&id).ok_or(DaemonError::InstanceNotFound(id))?;

        let clonable = matches!(
            record.state,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        );
        if !clonable {
            return Err(DaemonError::InstanceNotClonable(id, record.state.clone()));
        }

        if !matches!(record.config.kind, InstanceKind::AndroidVm { .. }) {
            return Err(DaemonError::CloneNotSupportedForKind(id));
        }

        Ok(record.config.clone())
    }

    /// Находит `InstanceId` всех инстансов, чей `disk.base_image`
    /// указывает прямо на диск инстанса `id` — то есть живых
    /// `CloneMode::Linked`-клонов этого инстанса (см. документацию
    /// `CloneMode::Linked` за тем, почему именно такие клоны, а не
    /// `FullStandalone`/`SharedBase`, представляют риск при purge).
    ///
    /// Сравнение по `disk.path` инстанса `id`, не по самому `id` —
    /// `base_image` в `InstanceConfig` хранит путь к файлу, не
    /// `InstanceId` (см. `DiskConfig`), это единственный способ найти
    /// связь без дополнительной, потенциально расходящейся с
    /// реальностью структуры данных только для этой цели. Стоимость —
    /// O(количество инстансов) сканирование при каждом
    /// `remove_instance(purge=true)`; на ожидаемых масштабах (десятки,
    /// не тысячи инстансов на один `andlerd`) это не узкое место,
    /// заводить отдельный индекс ради этого было бы преждевременной
    /// оптимизацией.
    ///
    /// Не различает `Linked`-клон от `SharedBase`-клона, у которого
    /// `base_image` совпал бы с диском источника лишь случайно (на
    /// практике невозможно: `SharedBase` всегда ссылается на общий
    /// `base_image` профиля, не на диск конкретного инстанса, — `id` и
    /// есть инстанс, не профиль, так что такое совпадение означало бы,
    /// что сам диск инстанса `id` используется как `base_image` целого
    /// профиля, что не предусмотрено текущей моделью создания
    /// Android-инстансов).
    async fn find_live_clones(&self, id: InstanceId) -> Result<Vec<InstanceId>, DaemonError> {
        let instances = self.instances.read().await;
        let target_disk_path = instances
            .get(&id)
            .map(|record| record.config.disk.path.clone())
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let clones = instances
            .iter()
            .filter(|(other_id, record)| {
                **other_id != id && record.config.disk.base_image.as_deref() == Some(target_disk_path.as_path())
            })
            .map(|(other_id, _)| *other_id)
            .collect();

        Ok(clones)
    }


    /// Запускает ранее созданный инстанс: `Created -> Starting -> Running`
    /// (см. `andler_core::fsm`). При ошибке backend'а переводит запись в
    /// `Error { message }`, а не оставляет её в промежуточном `Starting`
    /// — `Starting` без последующего `StartCompleted`/`Fail` означало бы
    /// зависшую запись, на которую не может среагировать ни одна другая
    /// операция (FSM не разрешает из `Starting` ничего, кроме `StartCompleted`
    /// и `Fail`, см. `fsm.rs`).
    ///
    /// Персистентность: промежуточное `Starting` сохраняется в `store`
    /// сразу же, до вызова `backend.spawn` — если процесс `andlerd`
    /// упадёт во время `spawn` (например, сам QEMU зависнет на старте),
    /// `restore` при следующем запуске должен увидеть `Starting`, а не
    /// устаревшее `Created`, и корректно перевести его в `Error` (см.
    /// `Daemon::restore`), а не предположить, что инстанс никогда не
    /// пытались запускать. Финальное состояние (`Running`/`Error`)
    /// сохраняется отдельно, уже после того, как write-lock `instances`
    /// отпущен — `persist_state` не должен выполняться, пока другие
    /// операции заблокированы на этом же инстансе.
    pub async fn start_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let starting_state = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get_mut(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            record.state = record.state.clone().apply(InstanceEvent::Start)?;
            record.state.clone()
        };
        self.persist_state(id, &starting_state).await;

        let (backend, cfg) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                self.backend_for(record.config.backend)?.clone(),
                record.config.clone(),
            )
        };

        let spawn_result = backend.spawn(&cfg).await;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let result = match spawn_result {
            Ok(handle) => {
                record.handle = Some(handle);
                record.state = record.state.clone().apply(InstanceEvent::StartCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        };
        let final_state = record.state.clone();
        drop(instances);

        self.persist_state(id, &final_state).await;
        result
    }

    /// Останавливает инстанс. `graceful` передаётся напрямую в
    /// `HypervisorBackend::stop` (см. его документацию про текущее
    /// значение "graceful" без полноценного ACPI-сигнала, пока в
    /// `andler-qemu` нет снапшота/более развитого qmp.rs).
    ///
    /// Персистентность — как в `start_instance`: промежуточное `Stopping`
    /// сохраняется сразу, финальное (`Stopped`/`Error`) — после того, как
    /// write-lock `instances` отпущен.
    pub async fn stop_instance(&self, id: InstanceId, graceful: bool) -> Result<(), DaemonError> {
        let (handle, backend, stopping_state) = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get_mut(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let handle = record.handle.clone().ok_or_else(|| {
                DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string()))
            })?;

            record.state = record.state.clone().apply(InstanceEvent::Stop)?;
            let backend = self.backend_for(record.config.backend)?.clone();

            (handle, backend, record.state.clone())
        };
        self.persist_state(id, &stopping_state).await;

        let stop_result = backend.stop(&handle, graceful).await;

        let mut instances = self.instances.write().await;
        let record = instances
            .get_mut(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let result = match stop_result {
            Ok(()) => {
                record.handle = None;
                record.state = record.state.clone().apply(InstanceEvent::StopCompleted)?;
                Ok(())
            }
            Err(backend_err) => {
                record.state = record
                    .state
                    .clone()
                    .apply(InstanceEvent::Fail(backend_err.to_string()))?;
                Err(DaemonError::Backend(backend_err))
            }
        };
        let final_state = record.state.clone();
        drop(instances);

        self.persist_state(id, &final_state).await;
        result
    }

    /// Приостанавливает работающий инстанс. В отличие от
    /// `start_instance`/`stop_instance`, не меняет `InstanceState` записи
    /// демона напрямую при успехе — синхронизация с реальным состоянием
    /// гостя (`Running`/`Paused`) происходит через `status()`, который
    /// спрашивает backend (а тот, в свою очередь, QMP `query-status`) о
    /// реальном состоянии, а не предполагает его исходя из того, что
    /// команда была отправлена успешно. Это сознательное решение: FSM
    /// `Daemon`-записи отражает то, что демон попросил сделать, а не то,
    /// что гость гарантированно уже сделал — при ошибке после успешной
    /// отправки команды (теоретическая гонка) `status()` всё равно покажет
    /// правду.
    pub async fn pause_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;
        let backend = self.backend_for(record.config.backend)?;

        backend.pause(&handle).await.map_err(DaemonError::Backend)
    }

    /// Возобновляет приостановленный инстанс. См. документацию
    /// `pause_instance` про то, почему `InstanceState` записи демона не
    /// меняется здесь напрямую.
    pub async fn resume_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = record
            .handle
            .clone()
            .ok_or_else(|| DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string())))?;
        let backend = self.backend_for(record.config.backend)?;

        backend.resume(&handle).await.map_err(DaemonError::Backend)
    }

    /// Текущий статус инстанса. Для инстансов без запущенного backend'а
    /// (ещё не стартовали, либо уже остановлены и `handle` сброшен)
    /// возвращает состояние FSM записи демона напрямую, без обращения к
    /// backend'у — у backend'а просто нет хэндла, который можно было бы
    /// спросить.
    pub async fn status(&self, id: InstanceId) -> Result<BackendStatus, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        match &record.handle {
            Some(handle) => {
                let backend = self.backend_for(record.config.backend)?;
                backend.status(handle).await.map_err(DaemonError::Backend)
            }
            None => Ok(BackendStatus {
                state: record.state.clone(),
                detail: Some("instance has no running backend handle".to_string()),
            }),
        }
    }

    /// Поток строк stdout/stderr процесса гипервизора инстанса (см.
    /// `andler_core::LogLine`) — live-tail с момента вызова, без истории
    /// (см. документацию `HypervisorBackend::log_stream` за обоснованием).
    ///
    /// Для инстанса без запущенного backend'а (`record.handle == None`,
    /// тот же случай, что у `status()` выше) возвращает немедленно
    /// завершающийся пустой поток, не ошибку — наблюдать за процессом,
    /// которого сейчас нет, означает "сейчас нечего показать", не
    /// "невалидный запрос" (см. документацию `HypervisorBackend::log_stream`
    /// за тем, почему это сознательно отличается от `pause_instance`/
    /// `resume_instance` с тем же отсутствующим хэндлом).
    ///
    /// Возвращаемый поток — `'static` и не заимствует `&self`: внутри
    /// держит собственный клон `Arc<dyn HypervisorBackend>` (backend'ы
    /// уже хранятся как `Arc` в `self.backends`, клонирование — это просто
    /// инкремент счётчика ссылок, не глубокое копирование) и сам
    /// `BackendHandle`, поэтому переживает возврат из этого метода — это
    /// необходимо: gRPC-хендлер (`andler-rpc`/`service.rs`) будет
    /// поллить этот поток уже после того, как вызов `stream_instance_logs`
    /// завершился и любые ссылки на `Daemon` из этого вызова вышли из
    /// скоупа.
    pub async fn stream_instance_logs(
        &self,
        id: InstanceId,
    ) -> Result<BoxStream<'static, LogLine>, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances
            .get(&id)
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let handle = match record.handle.clone() {
            Some(handle) => handle,
            None => return Ok(Box::pin(futures_util::stream::empty())),
        };
        let backend = self.backend_for(record.config.backend)?.clone();

        // `async_stream::stream!` строит `Stream`, которому позволено
        // владеть `backend`/`handle` внутри собственного тела генератора
        // — отсюда и `'static` результат, в отличие от прямого
        // `backend.log_stream(&handle)`, чей `BoxStream<'_, LogLine>`
        // заимствовал бы `backend` на время жизни этого вызова. Сам
        // внутренний поток подписывается на `broadcast`-канал процесса
        // только один раз, при первом полле (а не при каждом вызове
        // `log_stream`), и дальше просто транслирует его элементы —
        // подписка происходит ровно там же, где произошла бы при прямом
        // вызове `backend.log_stream(&handle)`.
        Ok(Box::pin(async_stream::stream! {
            let mut inner = backend.log_stream(&handle);
            while let Some(line) = inner.next().await {
                yield line;
            }
        }))
    }

    /// Сводка по всем зарегистрированным инстансам — единственный способ
    /// узнать, какие `InstanceId` вообще существуют, без необходимости
    /// заранее знать их (например, если вывод `andler create` потерян:
    /// закрыт терминал, не сохранён stdout скрипта — без `list_instances`
    /// созданный инстанс физически существует в `Daemon`/`Store`, но
    /// недостижим ни для одной другой команды, которым всем нужен
    /// `InstanceId` на входе).
    ///
    /// Сознательно возвращает только состояние записи демона
    /// (`record.state`), а не реальный backend-статус через
    /// `backend.status(handle)`, как делает `status()` для запущенных
    /// инстансов — обзорный список не должен порождать по одному
    /// сетевому/процессному запросу на каждый инстанс (для QEMU это QMP
    /// round-trip), когда вызывающему обычно нужен только список
    /// id/имён/грубых состояний, а не точный live-статус каждого. Для
    /// точного статуса конкретного инстанса остаётся `status(id)`.
    ///
    /// Порядок записей не гарантирован — источник (`HashMap`) сам по себе
    /// не упорядочен; вызывающая сторона (`andler-cli`) сортирует, если
    /// ей это нужно для вывода.
    pub async fn list_instances(&self) -> Vec<InstanceSummary> {
        let instances = self.instances.read().await;
        instances
            .values()
            .map(|record| InstanceSummary {
                id: record.config.id,
                name: record.config.name.clone(),
                state: record.state.clone(),
            })
            .collect()
    }

    /// Удаляет запись инстанса из `Daemon` (и из `store`, если
    /// персистентность включена).
    ///
    /// Запрещено для `Starting`/`Running`/`Paused`/`Stopping` —
    /// сознательно НЕ останавливает инстанс сначала сама: останавливать
    /// что-то от имени пользователя в рамках вызова, который выглядит как
    /// "удалить", было бы скрытым побочным эффектом (что если graceful
    /// stop важен пользователю и он не ожидал, что `remove` его выполнит
    /// неявно?). Вызывающая сторона должна сначала явно вызвать
    /// `stop_instance`, как и для `start_instance` на ещё не
    /// зарегистрированный инстанс — `Daemon` не угадывает намерение,
    /// требует явной последовательности операций. Разрешено из
    /// `Created`/`Stopped`/`Error` — не использует
    /// `InstanceState::is_terminal()` (тот определяет терминальность FSM:
    /// `Stopped | Error`, не включает `Created`), потому что семантика
    /// здесь другая — "безопасно ли удалить запись прямо сейчас", а не
    /// "достигнут ли конец графа переходов"; `Created` инстанс, который
    /// никогда не запускался, не имеет процесса, который можно было бы
    /// случайно оборвать удалением.
    ///
    /// Без `purge` (см. параметр) НЕ удаляет файлы инстанса с диска (диск,
    /// `instance_dir` для Android) — `InstanceConfig.disk.path` может
    /// указывать на путь, который пользователь указал сам (например, через
    /// `andler create --file`, см. `andler-cli`), и автоматическое удаление
    /// файла по произвольному пользовательскому пути — операция, которая
    /// не должна происходить неявно как побочный эффект удаления записи.
    /// Явное согласие пользователя на это — флаг `purge` (`andler remove
    /// --purge`), не часть этого вызова по умолчанию.
    ///
    /// Ошибка удаления из `store` логируется, но не проваливает операцию
    /// — как и в `persist_state`/`persist_new_instance`, in-memory
    /// состояние остаётся источником истины текущей сессии демона;
    /// разойдётся только переживание перезапуска.
    ///
    /// `purge: true` дополнительно удаляет файлы, однозначно принадлежащие
    /// только этому инстансу:
    /// - `config.disk.path` — для `AndroidVm` это overlay (никогда не
    ///   `base_image`, который кэширован и разделяется между инстансами —
    ///   его удалять нельзя в принципе, независимо от `purge`);
    /// - `config.firmware.ovmf_vars_path` — персональная копия EFI-переменных
    ///   (никогда `ovmf_code_path`, общий read-only образ всех инстансов).
    ///
    /// После удаления обоих файлов делается одна best-effort попытка
    /// `remove_dir` (не `remove_dir_all`!) родительского каталога
    /// `disk.path`: для `AndroidVm`, где оба файла лежат прямо в
    /// `instance_dir` и больше там ничего нет, каталог опустеет и будет
    /// убран; для `LinuxVm` с произвольным пользовательским путём каталог
    /// почти наверняка не пуст (там лежат чужие файлы пользователя) —
    /// `remove_dir` откажется удалять непустой каталог, и это тихо
    /// игнорируется. Не `remove_dir_all` — рекурсивное удаление
    /// родительского каталога произвольного пользовательского пути могло
    /// бы захватить файлы, не принадлежащие andler вообще.
    ///
    /// Ошибки самого удаления файлов (как и ошибка удаления из `store`)
    /// логируются, но не проваливают операцию — запись об инстансе уже
    /// удалена из `Daemon` и (если включена персистентность) из `store` к
    /// моменту попытки purge; превращать частичный сбой очистки диска в
    /// `Err` означало бы оставить вызывающую сторону с записью, которая
    /// выглядит неудалённой, хотя на самом деле уже удалена везде, кроме
    /// файловой системы.
    ///
    /// `purge: true` дополнительно отказывает целиком (запись НЕ
    /// удаляется, файлы НЕ трогаются), если у инстанса есть живые
    /// `CloneMode::Linked`-клоны (см. `find_live_clones`/
    /// `CloneMode::Linked`) — удаление `disk.path` сломало бы их
    /// `backing_file`. `FullStandalone`/`SharedBase`-клоны не создают
    /// такой зависимости и не блокируют purge. Без `purge` эта проверка
    /// не выполняется — запись исчезает из `Daemon`, но файл диска
    /// остаётся на месте, клоны не страдают.
    pub async fn remove_instance(&self, id: InstanceId, purge: bool) -> Result<(), DaemonError> {
        // Проверка живых клонов — только когда `purge: true` и только до
        // удаления записи (после `instances.remove(&id)` ниже у
        // `find_live_clones` не было бы доступа к `disk.path` источника,
        // см. её документацию). Без `purge` эта проверка не нужна:
        // запись исчезает из `Daemon`, но файл диска остаётся на месте
        // нетронутым — клон, ссылающийся на него как на `backing_file`,
        // ничего не замечает.
        if purge {
            let live_clones = self.find_live_clones(id).await?;
            if !live_clones.is_empty() {
                return Err(DaemonError::InstanceHasLiveClones(id, live_clones));
            }
        }

        let config = {
            let mut instances = self.instances.write().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let removable = matches!(
                record.state,
                InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
            );
            if !removable {
                return Err(DaemonError::InstanceNotRemovable(id, record.state.clone()));
            }

            instances.remove(&id).map(|record| record.config)
        };

        if let Some(store) = &self.store {
            if let Err(err) = store.delete_instance(id).await {
                tracing::error!(
                    instance_id = %id.0,
                    error = %err,
                    "failed to delete instance from store after in-memory removal"
                );
            }
        }

        if purge {
            if let Some(config) = config {
                purge_instance_files(id, &config).await;
            }
        }

        Ok(())
    }

    /// Возвращает полный `InstanceConfig` одного инстанса — в отличие от
    /// `list_instances` (только `id`/`name`/`state` на каждую запись), это
    /// весь конфиг целиком: CPU/память/диск/дисплей/GPU/сеть/firmware/
    /// audio/input. Существует отдельно от `list_instances` сознательно
    /// — большинству вызовов `list_instances` (например, выбор инстанса
    /// для дальнейшей команды) не нужен весь объём данных каждого
    /// инстанса, а `get_instance_config` — точечный запрос по одному
    /// `InstanceId`, когда конфиг действительно нужен целиком (например,
    /// чтобы показать пользователю, с чем именно был создан инстанс).
    ///
    /// Возвращает клон `InstanceConfig` (он уже `Clone` — см.
    /// `andler-core::config::instance`), не ссылку — вызывающая сторона
    /// (`service.rs`) должна владеть значением, чтобы сконвертировать его
    /// в proto-ответ уже после того, как read-lock `instances` отпущен.
    pub async fn get_instance_config(&self, id: InstanceId) -> Result<InstanceConfig, DaemonError> {
        let instances = self.instances.read().await;
        instances
            .get(&id)
            .map(|record| record.config.clone())
            .ok_or(DaemonError::InstanceNotFound(id))
    }
}

/// Одна строка сводки `Daemon::list_instances` — `id`/`name`/`state`, не
/// полный `InstanceConfig`: список существует для того, чтобы найти
/// нужный `InstanceId` и человекочитаемое имя, не для просмотра всей
/// конфигурации инстанса (для этого, если понадобится, естественнее
/// отдельный `GetInstanceConfig`, а не нагружать список лишним объёмом
/// данных, который в большинстве вызовов не нужен).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceKind, MemoryConfig, NetworkConfig, RenderBackend,
    };
    use andler_core::{AndroidVersion, RootMode};
    use std::path::PathBuf;

    /// Минимальная замена внешнему `tempfile` (не добавлен в
    /// dev-dependencies этого крейта): создаёт уникальный каталог в
    /// `std::env::temp_dir()` и удаляет его рекурсивно при выходе из
    /// скоупа теста (`Drop`, best-effort — ошибка очистки не паникует,
    /// она бы только замаскировала настоящий результат теста).
    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "andler-daemon-test-{}-{}",
                std::process::id(),
                InstanceId::new().0
            ));
            std::fs::create_dir_all(&path).expect("create test temp dir");
            TestTempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    /// Вариант `sample_config()` с `InstanceKind::AndroidVm` и явно
    /// заданными `disk.path`/`disk.base_image` — для тестов
    /// `clone_instance`/`find_live_clones`/`CloneNotSupportedForKind`,
    /// которым нужен именно `AndroidVm` с управляемым диском, но не
    /// нужен настоящий overlay-файл на диске (в отличие от
    /// `create_android_instance_resolves_profile_and_creates_overlay`,
    /// которая создаёт реальный qcow2 через `qemu-img` и помечена
    /// `#[ignore]`) — эти тесты вызывают `create_instance` напрямую,
    /// минуя `create_android_instance`, поэтому им не нужен бинарник
    /// `qemu-img` вообще, только корректные значения полей
    /// `InstanceConfig` для проверки логики `Daemon`, не файловой
    /// системы.
    fn sample_android_config(disk_path: PathBuf, base_image: PathBuf) -> InstanceConfig {
        let mut cfg = sample_config();
        cfg.id = InstanceId::new();
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: false,
                microg: false,
                libndk: false,
                root: RootMode::None,
            },
        };
        cfg.disk = DiskConfig::overlay(disk_path, base_image, 20 * DiskConfig::GIB);
        cfg
    }

    #[tokio::test]
    async fn create_instance_registers_with_created_state() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;

        let returned_id = daemon.create_instance(cfg).await.unwrap();
        assert_eq!(returned_id, id);

        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Created);
    }

    #[tokio::test]
    async fn status_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.status(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn start_instance_rejects_passthrough_via_backend_validation() {
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.start_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::InvalidConfig { .. })
        ));

        // Запись демона должна перейти в Error, не зависнуть в Starting.
        let status = daemon.status(id).await.unwrap();
        assert!(matches!(status.state, InstanceState::Error { .. }));
    }

    #[tokio::test]
    async fn pause_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.pause_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn pause_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.pause_instance(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn resume_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.resume_instance(id).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    /// В отличие от `pause_before_start_returns_handle_not_found` —
    /// `stream_instance_logs` без запущенного backend'а не должен быть
    /// ошибкой вообще (см. документацию метода за обоснованием): просто
    /// немедленно завершающийся пустой поток.
    #[tokio::test]
    async fn stream_logs_before_start_returns_empty_stream_not_error() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let mut stream = daemon.stream_instance_logs(id).await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_logs_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        // `.unwrap_err()` здесь не годится: оно требует `T: Debug` для
        // всего `Result<T, E>` (паническая ветка форматирует `Ok`-значение
        // через `{:?}`, даже когда мы ожидаем `Err`), а
        // `T = BoxStream<'static, LogLine>` = `Pin<Box<dyn Stream<...> +
        // Send>>` не реализует `Debug` и не может — оборачивать живой
        // поток в `Debug` не имеет смысла. `.err()` не требует `T: Debug`
        // (оно просто отбрасывает `Ok`-значение, не форматирует его).
        let err = daemon
            .stream_instance_logs(InstanceId::new())
            .await
            .err()
            .expect("an unregistered instance_id must yield an error, not a stream");
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn resume_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.resume_instance(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn stop_before_start_returns_handle_not_found() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon.stop_instance(id, true).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::Backend(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn double_create_with_same_id_overwrites_record() {
        // Документирует текущее поведение: create_instance не проверяет
        // существование записи и просто перезаписывает её — это осознанно
        // не валидируется здесь, так как InstanceId генерируется через
        // InstanceId::new() (UUID v4) и коллизия при нормальном
        // использовании практически невозможна; явная защита от повторного
        // create с тем же ID добавится, если/когда появится API, которое
        // действительно может передать чужой ID (например, восстановление
        // из andler-store).
        let daemon = Daemon::new();
        let cfg1 = sample_config();
        let id = cfg1.id;
        daemon.create_instance(cfg1).await.unwrap();

        let mut cfg2 = sample_config();
        cfg2.id = id;
        cfg2.name = "renamed".to_string();
        daemon.create_instance(cfg2).await.unwrap();

        let instances = daemon.instances.read().await;
        assert_eq!(instances.get(&id).unwrap().config.name, "renamed");
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_android_instance_resolves_profile_and_creates_overlay() {
        let dir = std::env::temp_dir().join("andler-daemon-test-android-e2e");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Базовый образ и шаблон OVMF_VARS — оба "настоящие" файлы на диске,
        // как и в реальной системе (только без реального содержимого UEFI
        // vars — для qemu-img create это не важно).
        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
            .await
            .unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            libndk: true,
            root: RootMode::None,
        };

        let id = daemon
            .create_android_instance(
                profile.clone(),
                "my-android".to_string(),
                base_image.clone(),
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Created);

        let instance_dir = instances_root.join(id.0.to_string());
        assert!(instance_dir.join("disk.qcow2").exists());
        assert!(instance_dir.join("VARS.fd").exists());

        let instances = daemon.instances.read().await;
        let record = instances.get(&id).unwrap();
        assert_eq!(record.config.id, id);
        assert_eq!(record.config.disk.base_image, Some(base_image));
        match &record.config.kind {
            InstanceKind::AndroidVm { android_profile } => {
                assert_eq!(*android_profile, profile);
            }
            InstanceKind::LinuxVm { .. } => panic!("expected AndroidVm"),
        }

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn create_android_instance_fails_when_base_image_missing() {
        let dir = std::env::temp_dir().join("andler-daemon-test-android-missing-base");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars")
            .await
            .unwrap();

        let missing_base = dir.join("does-not-exist.qcow2");
        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: true,
            libndk: false,
            root: RootMode::None,
        };

        let err = daemon
            .create_android_instance(
                profile,
                "my-android".to_string(),
                missing_base,
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            DaemonError::Disk(andler_disk::DiskError::BackingFileNotFound(_))
        ));

        // InstanceDirGuard должен убрать за собой: к моменту сбоя на
        // create_overlay каталог инстанса и скопированный VARS.fd уже
        // были созданы на диске (см. порядок операций в
        // create_android_instance) — без очистки они остались бы
        // мусором, не привязанным ни к одной зарегистрированной записи.
        let mut instance_dirs = tokio::fs::read_dir(&instances_root).await.unwrap();
        assert!(
            instance_dirs.next_entry().await.unwrap().is_none(),
            "instances_root must be empty after a failed create_android_instance"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn create_android_instance_cleans_up_instance_dir_on_missing_ovmf_template() {
        // Сбой раньше, чем в create_android_instance_fails_when_base_image_missing:
        // здесь падает само copy(ovmf_vars_template) — instance_dir к
        // этому моменту уже создан (пустой, без VARS.fd), и его тоже
        // нужно убрать.
        let dir = std::env::temp_dir().join("andler-daemon-test-android-missing-ovmf");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let missing_ovmf_template = dir.join("does-not-exist-template.fd");
        let base_image = dir.join("base.qcow2"); // не используется, copy падает раньше
        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: true,
            libndk: false,
            root: RootMode::None,
        };

        let err = daemon
            .create_android_instance(
                profile,
                "my-android".to_string(),
                base_image,
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                missing_ovmf_template,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, DaemonError::Io { .. }));

        let mut instance_dirs = tokio::fs::read_dir(&instances_root).await.unwrap();
        assert!(
            instance_dirs.next_entry().await.unwrap().is_none(),
            "instances_root must be empty after copy(ovmf_vars_template) fails"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    // --- Персистентность (andler-store) -----------------------------

    #[tokio::test]
    async fn with_store_persists_created_instance() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let daemon = Daemon::with_store(store.clone());
        let cfg = sample_config();
        let id = cfg.id;

        daemon.create_instance(cfg.clone()).await.unwrap();

        let stored = store.load_instance(id).await.unwrap();
        assert_eq!(stored.config, cfg);
        assert_eq!(stored.state, InstanceState::Created);
    }

    #[tokio::test]
    async fn with_store_persists_failed_start_as_error_state() {
        // start_instance с RenderBackend::Passthrough гарантированно падает
        // на валидации backend'а (см.
        // start_instance_rejects_passthrough_via_backend_validation выше) —
        // удобный способ проверить персистентность именно ветки Fail без
        // реального QEMU-процесса.
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let daemon = Daemon::with_store(store.clone());
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        daemon.start_instance(id).await.unwrap_err();

        let stored = store.load_instance(id).await.unwrap();
        assert!(matches!(stored.state, InstanceState::Error { .. }));
    }

    #[tokio::test]
    async fn daemon_without_store_does_not_panic_on_state_transitions() {
        // Daemon::new() (store: None) должен продолжать работать как
        // раньше — persist_state/persist_new_instance должны быть no-op,
        // а не паниковать на отсутствующем Store.
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();
        daemon.start_instance(id).await.unwrap_err();

        let status = daemon.status(id).await.unwrap();
        assert!(matches!(status.state, InstanceState::Error { .. }));
    }

    #[tokio::test]
    async fn restore_recreates_daemon_from_store_contents() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let daemon = Daemon::restore(store).await.unwrap();

        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Created);
    }

    #[tokio::test]
    async fn restore_marks_running_instance_as_error_since_handle_is_lost() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Running)
            .await
            .unwrap();

        let daemon = Daemon::restore(store).await.unwrap();

        let status = daemon.status(id).await.unwrap();
        assert!(
            matches!(status.state, InstanceState::Error { .. }),
            "expected Running to be restored as Error, got {:?}",
            status.state
        );
    }

    #[tokio::test]
    async fn restore_marks_every_non_terminal_state_as_error() {
        for lost_state in [
            InstanceState::Starting,
            InstanceState::Running,
            InstanceState::Paused,
            InstanceState::Stopping,
        ] {
            let store = andler_store::Store::open_in_memory().await.unwrap();
            let cfg = sample_config();
            let id = cfg.id;
            store.save_instance(&cfg, &lost_state).await.unwrap();

            let daemon = Daemon::restore(store).await.unwrap();
            let status = daemon.status(id).await.unwrap();
            assert!(
                matches!(status.state, InstanceState::Error { .. }),
                "expected {lost_state:?} to be restored as Error, got {:?}",
                status.state
            );
        }
    }

    #[tokio::test]
    async fn restore_keeps_terminal_states_unchanged() {
        for terminal_state in [
            InstanceState::Created,
            InstanceState::Stopped,
            InstanceState::Error {
                message: "previous failure".to_string(),
            },
        ] {
            let store = andler_store::Store::open_in_memory().await.unwrap();
            let cfg = sample_config();
            let id = cfg.id;
            store
                .save_instance(&cfg, &terminal_state)
                .await
                .unwrap();

            let daemon = Daemon::restore(store).await.unwrap();
            let status = daemon.status(id).await.unwrap();
            assert_eq!(
                status.state, terminal_state,
                "expected terminal state {terminal_state:?} to survive restore unchanged"
            );
        }
    }

    #[tokio::test]
    async fn restore_on_empty_store_yields_daemon_with_no_instances() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let daemon = Daemon::restore(store).await.unwrap();

        let err = daemon.status(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn restored_daemon_continues_to_persist_further_transitions() {
        // Не просто "restore работает один раз" — restored Daemon должен
        // оставаться полностью функциональным Daemon::with_store: новые
        // create_instance/start_instance после restore должны продолжать
        // попадать в тот же store.
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let daemon = Daemon::restore(store.clone()).await.unwrap();

        let cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg.clone()).await.unwrap();

        let stored = store.load_instance(id).await.unwrap();
        assert_eq!(stored.config, cfg);
        assert_eq!(stored.state, InstanceState::Created);
    }

    // --- list_instances ------------------------------------------------

    #[tokio::test]
    async fn list_instances_on_empty_daemon_returns_empty_vec() {
        let daemon = Daemon::new();
        assert!(daemon.list_instances().await.is_empty());
    }

    #[tokio::test]
    async fn list_instances_returns_one_summary_per_created_instance() {
        let daemon = Daemon::new();
        let cfg1 = sample_config();
        let mut cfg2 = sample_config();
        cfg2.name = "second-vm".to_string();

        let id1 = daemon.create_instance(cfg1).await.unwrap();
        let id2 = daemon.create_instance(cfg2).await.unwrap();

        let mut summaries = daemon.list_instances().await;
        summaries.sort_by_key(|s| s.name.clone());

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, id2);
        assert_eq!(summaries[0].name, "second-vm");
        assert_eq!(summaries[0].state, InstanceState::Created);
        assert_eq!(summaries[1].id, id1);
        assert_eq!(summaries[1].state, InstanceState::Created);
    }

    #[tokio::test]
    async fn list_instances_reflects_state_after_failed_start() {
        // list_instances должен видеть актуальное record.state, не
        // застывший снимок на момент create_instance — после неудачного
        // start_instance (Passthrough, как и в других тестах на Fail-ветку
        // выше) запись должна появиться в списке уже как Error, не
        // Created.
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = daemon.create_instance(cfg).await.unwrap();
        daemon.start_instance(id).await.unwrap_err();

        let summaries = daemon.list_instances().await;
        assert_eq!(summaries.len(), 1);
        assert!(matches!(summaries[0].state, InstanceState::Error { .. }));
    }

    #[tokio::test]
    async fn list_instances_after_restore_includes_restored_instances() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let cfg = sample_config();
        let id = cfg.id;
        store
            .save_instance(&cfg, &InstanceState::Created)
            .await
            .unwrap();

        let daemon = Daemon::restore(store).await.unwrap();
        let summaries = daemon.list_instances().await;

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, id);
        assert_eq!(summaries[0].name, cfg.name);
    }

    // --- remove_instance -------------------------------------------------

    #[tokio::test]
    async fn remove_instance_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.remove_instance(InstanceId::new(), false).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn remove_instance_succeeds_from_created() {
        let daemon = Daemon::new();
        let id = daemon.create_instance(sample_config()).await.unwrap();

        daemon.remove_instance(id, false).await.unwrap();

        let err = daemon.status(id).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn remove_instance_succeeds_from_error_state() {
        // Created -> Error через тот же неудачный-start трюк, что в
        // других тестах на ветку Fail (RenderBackend::Passthrough
        // гарантированно падает на валидации backend'а без реального
        // QEMU-процесса).
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let id = daemon.create_instance(cfg).await.unwrap();
        daemon.start_instance(id).await.unwrap_err();

        daemon.remove_instance(id, false).await.unwrap();

        let err = daemon.status(id).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn remove_instance_rejects_running_instance() {
        // Получить реальный Running через start_instance() требует
        // настоящего qemu-system-x86_64/доступа к /dev/kvm, которых нет в
        // обычном unit-test окружении (см. #[ignore]-тесты в
        // andler-qemu::backend). Вместо этого вставляем запись с нужным
        // state напрямую — тестовый модуль того же файла имеет доступ к
        // приватным `Daemon::instances`/`InstanceRecord`, и это не
        // обходит проверяемую логику: remove_instance видит только
        // record.state, ему не важно, как именно запись попала в Running.
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.instances.write().await.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Running,
                handle: None,
            },
        );

        let err = daemon.remove_instance(id, false).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::InstanceNotRemovable(_, InstanceState::Running)
        ));

        // Запись должна остаться нетронутой после отказа — remove_instance
        // не должен ничего удалять/менять, если проверка removable не
        // прошла.
        let status = daemon.status(id).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);
    }

    #[tokio::test]
    async fn remove_instance_rejects_every_non_terminal_state() {
        for state in [
            InstanceState::Starting,
            InstanceState::Running,
            InstanceState::Paused,
            InstanceState::Stopping,
        ] {
            let daemon = Daemon::new();
            let cfg = sample_config();
            let id = cfg.id;
            daemon.instances.write().await.insert(
                id,
                InstanceRecord {
                    config: cfg,
                    state: state.clone(),
                    handle: None,
                },
            );

            let err = daemon.remove_instance(id, false).await.unwrap_err();
            assert!(
                matches!(err, DaemonError::InstanceNotRemovable(_, ref s) if *s == state),
                "expected InstanceNotRemovable({state:?}, ..), got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn remove_instance_succeeds_from_stopped() {
        // Stopped — терминальное состояние, достижимое только через
        // успешный stop_instance() на реально запущенном инстансе в
        // обычной работе демона; здесь инъектируем его напрямую по той
        // же причине, что в remove_instance_rejects_running_instance —
        // remove_instance проверяет только record.state, не то, как
        // инстанс в это состояние попал.
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = cfg.id;
        daemon.instances.write().await.insert(
            id,
            InstanceRecord {
                config: cfg,
                state: InstanceState::Stopped,
                handle: None,
            },
        );

        daemon.remove_instance(id, false).await.unwrap();

        let err = daemon.status(id).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn remove_instance_also_deletes_from_store() {
        let store = andler_store::Store::open_in_memory().await.unwrap();
        let daemon = Daemon::with_store(store.clone());
        let id = daemon.create_instance(sample_config()).await.unwrap();

        daemon.remove_instance(id, false).await.unwrap();

        let err = store.load_instance(id).await.unwrap_err();
        assert!(matches!(err, andler_store::StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn remove_instance_disappears_from_list_instances() {
        let daemon = Daemon::new();
        let id = daemon.create_instance(sample_config()).await.unwrap();
        daemon.remove_instance(id, false).await.unwrap();

        let summaries = daemon.list_instances().await;
        assert!(summaries.is_empty());
    }

    #[tokio::test]
    async fn remove_instance_without_purge_leaves_disk_and_firmware_files() {
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("disk.qcow2");
        let vars_path = dir.path().join("VARS.fd");
        tokio::fs::write(&disk_path, b"disk").await.unwrap();
        tokio::fs::write(&vars_path, b"vars").await.unwrap();

        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.disk.path = disk_path.clone();
        cfg.firmware.ovmf_vars_path = vars_path.clone();
        let id = daemon.create_instance(cfg).await.unwrap();

        daemon.remove_instance(id, false).await.unwrap();

        assert!(disk_path.exists());
        assert!(vars_path.exists());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_deletes_disk_and_firmware_files() {
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("disk.qcow2");
        let vars_path = dir.path().join("VARS.fd");
        tokio::fs::write(&disk_path, b"disk").await.unwrap();
        tokio::fs::write(&vars_path, b"vars").await.unwrap();

        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.disk.path = disk_path.clone();
        cfg.firmware.ovmf_vars_path = vars_path.clone();
        let id = daemon.create_instance(cfg).await.unwrap();

        daemon.remove_instance(id, true).await.unwrap();

        assert!(!disk_path.exists());
        assert!(!vars_path.exists());
        // Каталог состоял только из этих двух файлов -> должен опустеть и
        // быть убран best-effort `remove_dir`.
        assert!(!dir.path().exists());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_keeps_non_empty_parent_directory() {
        // Имитирует LinuxVm с пользовательским путём: рядом с диском лежит
        // чужой файл, не принадлежащий andler — purge должен удалить только
        // disk.path/ovmf_vars_path, но не трогать каталог целиком.
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("disk.qcow2");
        let vars_path = dir.path().join("VARS.fd");
        let unrelated_path = dir.path().join("unrelated-user-file.txt");
        tokio::fs::write(&disk_path, b"disk").await.unwrap();
        tokio::fs::write(&vars_path, b"vars").await.unwrap();
        tokio::fs::write(&unrelated_path, b"mine").await.unwrap();

        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.disk.path = disk_path.clone();
        cfg.firmware.ovmf_vars_path = vars_path.clone();
        let id = daemon.create_instance(cfg).await.unwrap();

        daemon.remove_instance(id, true).await.unwrap();

        assert!(!disk_path.exists());
        assert!(!vars_path.exists());
        assert!(dir.path().exists());
        assert!(unrelated_path.exists());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_never_deletes_shared_base_image_or_ovmf_code() {
        // base_image (кэшированный, общий для нескольких AndroidVm) и
        // ovmf_code_path (общий read-only образ всех инстансов) не
        // принадлежат конкретному инстансу — purge не должен их трогать,
        // даже если они существуют на диске.
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("overlay.qcow2");
        let vars_path = dir.path().join("VARS.fd");
        let base_image_path = dir.path().join("base.qcow2");
        let ovmf_code_path = dir.path().join("OVMF_CODE.fd");
        for path in [&disk_path, &vars_path, &base_image_path, &ovmf_code_path] {
            tokio::fs::write(path, b"data").await.unwrap();
        }

        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.disk.path = disk_path.clone();
        cfg.disk.base_image = Some(base_image_path.clone());
        cfg.firmware.ovmf_vars_path = vars_path.clone();
        cfg.firmware.ovmf_code_path = ovmf_code_path.clone();
        let id = daemon.create_instance(cfg).await.unwrap();

        daemon.remove_instance(id, true).await.unwrap();

        assert!(!disk_path.exists());
        assert!(!vars_path.exists());
        assert!(base_image_path.exists());
        assert!(ovmf_code_path.exists());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_tolerates_already_missing_files() {
        // Файлы могли быть удалены вручную пользователем до remove --purge
        // — purge должен остаться best-effort и не провалить операцию.
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("disk.qcow2");
        let vars_path = dir.path().join("VARS.fd");
        // Не создаём файлы вообще — каталог тоже не существует.

        let daemon = Daemon::new();
        let mut cfg = sample_config();
        cfg.disk.path = disk_path;
        cfg.firmware.ovmf_vars_path = vars_path;
        let id = daemon.create_instance(cfg).await.unwrap();

        daemon.remove_instance(id, true).await.unwrap();

        let err = daemon.status(id).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    // --- get_instance_config ---------------------------------------------

    #[tokio::test]
    async fn get_instance_config_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon
            .get_instance_config(InstanceId::new())
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn get_instance_config_returns_full_config_unchanged() {
        let daemon = Daemon::new();
        let cfg = sample_config();
        let id = daemon.create_instance(cfg.clone()).await.unwrap();

        let fetched = daemon.get_instance_config(id).await.unwrap();
        assert_eq!(fetched, cfg);
    }

    #[tokio::test]
    async fn get_instance_config_reflects_current_record_not_a_stale_snapshot() {
        // Не просто "вернуть то, что было передано в create_instance" —
        // get_instance_config должен видеть актуальную запись из
        // instances, даже если у инстанса с тех пор сменился id записи
        // через double_create_with_same_id_overwrites_record-сценарий
        // (см. соседний тест на это поведение Daemon::create_instance).
        let daemon = Daemon::new();
        let mut cfg = sample_config();
        let id = cfg.id;
        daemon.create_instance(cfg.clone()).await.unwrap();

        cfg.name = "renamed-vm".to_string();
        daemon.create_instance(cfg.clone()).await.unwrap();

        let fetched = daemon.get_instance_config(id).await.unwrap();
        assert_eq!(fetched.name, "renamed-vm");
    }

    // --- clone_instance / export_instance_disk / find_live_clones ---
    //
    // Тесты ниже регистрируют записи через `create_instance` напрямую
    // (не через `create_android_instance`), поэтому им не нужен
    // настоящий overlay-файл/бинарник `qemu-img` для проверки логики
    // `Daemon` (выбор режима, FSM-проверки, поиск живых клонов) — кроме
    // тех, что явно вызывают `clone_instance`/`export_instance_disk`
    // целиком (они доходят до `andler_disk::clone::*`, которому нужен
    // настоящий `qemu-img`, и помечены `#[ignore]`, как и аналогичные
    // тесты `create_android_instance`).

    #[tokio::test]
    async fn clone_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon
            .clone_instance(
                InstanceId::new(),
                "clone".to_string(),
                PathBuf::from("/tmp/instances"),
                CloneMode::Linked,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn clone_rejects_linux_vm_with_clone_not_supported_for_kind() {
        let daemon = Daemon::new();
        let cfg = sample_config(); // LinuxVm
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon
            .clone_instance(
                id,
                "clone".to_string(),
                PathBuf::from("/tmp/instances"),
                CloneMode::Linked,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonError::CloneNotSupportedForKind(returned_id) if returned_id == id));
    }

    #[tokio::test]
    async fn clone_rejects_non_terminal_source_state() {
        let dir = TestTempDir::new();
        let daemon = Daemon::new();
        let cfg = sample_android_config(
            dir.path().join("disk.qcow2"),
            dir.path().join("base.qcow2"),
        );
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        // pause_before_start_returns_handle_not_found показывает, что
        // start_instance без реального backend'а перейдёт в Error — это
        // нетерминальное-для-clone не является целью здесь; вместо
        // этого напрямую переводим запись в Running через тот же приём,
        // что и в других тестах этого файла, проверяющих поведение при
        // нетерминальных состояниях (см. start_instance_rejects_*) —
        // конкретно, манипулируем state напрямую через write-lock, так
        // как нет смысла гонять настоящий QemuBackend только чтобы
        // получить Running.
        {
            let mut instances = daemon.instances.write().await;
            let record = instances.get_mut(&id).unwrap();
            record.state = InstanceState::Running;
        }

        let err = daemon
            .clone_instance(
                id,
                "clone".to_string(),
                dir.path().to_path_buf(),
                CloneMode::Linked,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            DaemonError::InstanceNotClonable(returned_id, InstanceState::Running)
                if returned_id == id
        ));
    }

    #[tokio::test]
    async fn export_on_linux_vm_returns_clone_not_supported_for_kind() {
        let daemon = Daemon::new();
        let cfg = sample_config(); // LinuxVm
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let err = daemon
            .export_instance_disk(id, PathBuf::from("/tmp/export.qcow2"))
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonError::CloneNotSupportedForKind(returned_id) if returned_id == id));
    }

    #[tokio::test]
    async fn find_live_clones_on_unknown_instance_returns_instance_not_found() {
        let daemon = Daemon::new();
        let err = daemon.find_live_clones(InstanceId::new()).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceNotFound(_)));
    }

    #[tokio::test]
    async fn find_live_clones_is_empty_when_nothing_references_the_disk() {
        let dir = TestTempDir::new();
        let daemon = Daemon::new();
        let cfg = sample_android_config(
            dir.path().join("disk.qcow2"),
            dir.path().join("base.qcow2"),
        );
        let id = cfg.id;
        daemon.create_instance(cfg).await.unwrap();

        let clones = daemon.find_live_clones(id).await.unwrap();
        assert!(clones.is_empty());
    }

    #[tokio::test]
    async fn find_live_clones_finds_instance_whose_base_image_is_the_source_disk() {
        let dir = TestTempDir::new();
        let daemon = Daemon::new();

        let source_disk = dir.path().join("source").join("disk.qcow2");
        let source_cfg =
            sample_android_config(source_disk.clone(), dir.path().join("base.qcow2"));
        let source_id = source_cfg.id;
        daemon.create_instance(source_cfg).await.unwrap();

        // Имитирует результат CloneMode::Linked: новый инстанс с
        // disk.base_image == путь к диску источника (не к общему
        // base_image профиля) — см. документацию find_live_clones за
        // тем, почему именно это поле и есть сигнал "живой Linked-клон".
        let linked_clone_cfg =
            sample_android_config(dir.path().join("clone").join("disk.qcow2"), source_disk.clone());
        let clone_id = linked_clone_cfg.id;
        daemon.create_instance(linked_clone_cfg).await.unwrap();

        let clones = daemon.find_live_clones(source_id).await.unwrap();
        assert_eq!(clones, vec![clone_id]);
    }

    #[tokio::test]
    async fn find_live_clones_ignores_instances_sharing_only_the_base_image() {
        // Два обычных AndroidVm-инстанса одного профиля (оба overlay от
        // одного base_image, без отношения клон-источник друг к другу)
        // не должны считаться клонами друг друга — у обоих
        // disk.base_image указывает на общий профильный образ, не на
        // диск друг друга.
        let dir = TestTempDir::new();
        let daemon = Daemon::new();
        let shared_base_image = dir.path().join("base.qcow2");

        let first_cfg = sample_android_config(
            dir.path().join("first").join("disk.qcow2"),
            shared_base_image.clone(),
        );
        let first_id = first_cfg.id;
        daemon.create_instance(first_cfg).await.unwrap();

        let second_cfg = sample_android_config(
            dir.path().join("second").join("disk.qcow2"),
            shared_base_image,
        );
        daemon.create_instance(second_cfg).await.unwrap();

        let clones = daemon.find_live_clones(first_id).await.unwrap();
        assert!(clones.is_empty());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_rejects_when_live_linked_clone_exists() {
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("source").join("disk.qcow2");
        let vars_path = dir.path().join("source").join("VARS.fd");
        tokio::fs::create_dir_all(disk_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&disk_path, b"disk").await.unwrap();
        tokio::fs::write(&vars_path, b"vars").await.unwrap();

        let daemon = Daemon::new();
        let mut source_cfg =
            sample_android_config(disk_path.clone(), dir.path().join("base.qcow2"));
        source_cfg.firmware.ovmf_vars_path = vars_path.clone();
        let source_id = source_cfg.id;
        daemon.create_instance(source_cfg).await.unwrap();

        let clone_cfg =
            sample_android_config(dir.path().join("clone").join("disk.qcow2"), disk_path.clone());
        let clone_id = clone_cfg.id;
        daemon.create_instance(clone_cfg).await.unwrap();

        let err = daemon.remove_instance(source_id, true).await.unwrap_err();
        assert!(matches!(
            err,
            DaemonError::InstanceHasLiveClones(returned_id, ref clones)
                if returned_id == source_id && clones == &vec![clone_id]
        ));

        // Отказ — значит ничего не должно было быть тронуто: ни файлы,
        // ни сама запись.
        assert!(disk_path.exists());
        assert!(vars_path.exists());
        assert!(daemon.status(source_id).await.is_ok());
    }

    #[tokio::test]
    async fn remove_instance_with_purge_succeeds_when_clone_is_full_standalone() {
        // FullStandalone/SharedBase-клоны не создают зависимости от
        // источника (см. документацию CloneMode) — их существование не
        // должно блокировать purge источника, в отличие от Linked.
        let dir = TestTempDir::new();
        let disk_path = dir.path().join("source").join("disk.qcow2");
        let vars_path = dir.path().join("source").join("VARS.fd");
        tokio::fs::create_dir_all(disk_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&disk_path, b"disk").await.unwrap();
        tokio::fs::write(&vars_path, b"vars").await.unwrap();

        let daemon = Daemon::new();
        let mut source_cfg =
            sample_android_config(disk_path.clone(), dir.path().join("base.qcow2"));
        source_cfg.firmware.ovmf_vars_path = vars_path.clone();
        let source_id = source_cfg.id;
        daemon.create_instance(source_cfg).await.unwrap();

        // FullStandalone-клон: disk.base_image == None, не путь к
        // источнику — find_live_clones не должен его найти.
        let mut standalone_clone_cfg =
            sample_android_config(dir.path().join("clone").join("disk.qcow2"), disk_path.clone());
        standalone_clone_cfg.disk.base_image = None;
        daemon.create_instance(standalone_clone_cfg).await.unwrap();

        daemon.remove_instance(source_id, true).await.unwrap();

        assert!(!disk_path.exists());
        assert!(!vars_path.exists());
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn clone_instance_with_linked_mode_creates_overlay_pointing_at_source_disk() {
        let dir = std::env::temp_dir().join("andler-daemon-test-clone-linked");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            libndk: false,
            root: RootMode::None,
        };
        let source_id = daemon
            .create_android_instance(
                profile,
                "source".to_string(),
                base_image.clone(),
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();
        let source_disk_path = daemon
            .get_instance_config(source_id)
            .await
            .unwrap()
            .disk
            .path;

        let clone_id = daemon
            .clone_instance(
                source_id,
                "clone-of-source".to_string(),
                instances_root.clone(),
                CloneMode::Linked,
            )
            .await
            .unwrap();

        let clone_cfg = daemon.get_instance_config(clone_id).await.unwrap();
        assert_eq!(clone_cfg.name, "clone-of-source");
        assert_eq!(clone_cfg.disk.base_image, Some(source_disk_path.clone()));
        assert!(clone_cfg.disk.path.exists());
        assert_ne!(clone_cfg.disk.path, source_disk_path);
        assert!(clone_cfg.firmware.ovmf_vars_path.exists());
        assert_ne!(
            clone_cfg.firmware.ovmf_vars_path,
            daemon.get_instance_config(source_id).await.unwrap().firmware.ovmf_vars_path
        );

        // Источник теперь имеет живой Linked-клон — purge должен
        // отказать (сквозная проверка end-to-end того же поведения, что
        // уже проверено изолированно в
        // remove_instance_with_purge_rejects_when_live_linked_clone_exists).
        let err = daemon.remove_instance(source_id, true).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceHasLiveClones(_, _)));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn clone_instance_with_full_standalone_mode_has_no_base_image() {
        let dir = std::env::temp_dir().join("andler-daemon-test-clone-standalone");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            libndk: false,
            root: RootMode::None,
        };
        let source_id = daemon
            .create_android_instance(
                profile,
                "source".to_string(),
                base_image,
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let clone_id = daemon
            .clone_instance(
                source_id,
                "standalone-clone".to_string(),
                instances_root,
                CloneMode::FullStandalone,
            )
            .await
            .unwrap();

        let clone_cfg = daemon.get_instance_config(clone_id).await.unwrap();
        assert_eq!(clone_cfg.disk.base_image, None);
        assert!(clone_cfg.disk.path.exists());

        // FullStandalone-клон не должен блокировать purge источника.
        daemon.remove_instance(source_id, true).await.unwrap();

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn clone_instance_with_shared_base_mode_survives_source_purge() {
        let dir = std::env::temp_dir().join("andler-daemon-test-clone-shared-base");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            libndk: false,
            root: RootMode::None,
        };
        let source_id = daemon
            .create_android_instance(
                profile,
                "source".to_string(),
                base_image.clone(),
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let clone_id = daemon
            .clone_instance(
                source_id,
                "shared-base-clone".to_string(),
                instances_root,
                CloneMode::SharedBase,
            )
            .await
            .unwrap();

        let clone_cfg_before = daemon.get_instance_config(clone_id).await.unwrap();
        assert_eq!(clone_cfg_before.disk.base_image, Some(base_image));
        assert!(clone_cfg_before.disk.path.exists());

        // SharedBase-клон не зависит от источника физически — purge
        // источника (включая удаление файла его диска) не должен
        // повредить уже скопированный файл клона.
        daemon.remove_instance(source_id, true).await.unwrap();
        assert!(clone_cfg_before.disk.path.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn clone_instance_of_a_clone_is_allowed() {
        // Подтверждает решение "клонировать клон разрешено" — двухуровневая
        // Linked-цепочка (source -> clone_a -> clone_b) не требует
        // особого случая в clone_instance, см. её документацию.
        let dir = std::env::temp_dir().join("andler-daemon-test-clone-of-clone");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            libndk: false,
            root: RootMode::None,
        };
        let source_id = daemon
            .create_android_instance(
                profile,
                "source".to_string(),
                base_image,
                instances_root.clone(),
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let clone_a_id = daemon
            .clone_instance(
                source_id,
                "clone-a".to_string(),
                instances_root.clone(),
                CloneMode::Linked,
            )
            .await
            .unwrap();

        let clone_b_id = daemon
            .clone_instance(
                clone_a_id,
                "clone-b".to_string(),
                instances_root,
                CloneMode::Linked,
            )
            .await
            .unwrap();

        let clone_a_disk = daemon.get_instance_config(clone_a_id).await.unwrap().disk.path;
        let clone_b_cfg = daemon.get_instance_config(clone_b_id).await.unwrap();
        assert_eq!(clone_b_cfg.disk.base_image, Some(clone_a_disk.clone()));

        // clone_a теперь имеет свой собственный живой клон (clone_b) —
        // purge clone_a должен отказать по той же причине, что у source.
        let err = daemon.remove_instance(clone_a_id, true).await.unwrap_err();
        assert!(matches!(err, DaemonError::InstanceHasLiveClones(returned_id, ref clones)
            if returned_id == clone_a_id && clones == &vec![clone_b_id]));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn export_instance_disk_creates_standalone_file_without_registering_instance() {
        let dir = std::env::temp_dir().join("andler-daemon-test-export");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let base_image = dir.join("base.qcow2");
        andler_disk::qcow2::create(&base_image, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let ovmf_template = dir.join("OVMF_VARS.template.fd");
        tokio::fs::write(&ovmf_template, b"fake-ovmf-vars").await.unwrap();

        let instances_root = dir.join("instances");
        let daemon = Daemon::new();

        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: false,
            microg: false,
            libndk: false,
            root: RootMode::None,
        };
        let source_id = daemon
            .create_android_instance(
                profile,
                "source".to_string(),
                base_image,
                instances_root,
                20 * 1024 * 1024 * 1024,
                ovmf_template,
            )
            .await
            .unwrap();

        let export_path = dir.join("exported.qcow2");
        daemon
            .export_instance_disk(source_id, export_path.clone())
            .await
            .unwrap();

        assert!(export_path.exists());

        // list_instances не должен видеть никакой новой записи —
        // экспорт не регистрирует инстанс.
        let instances_before = daemon.list_instances().await;
        assert_eq!(instances_before.len(), 1);
        assert_eq!(instances_before[0].id, source_id);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
