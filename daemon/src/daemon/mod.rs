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

mod error;
mod types;
mod snapshot_ops;
mod query_ops;
mod instance_ops;
mod clone_ops;
mod health_ops;

pub use error::DaemonError;
pub(crate) use types::InstanceRecord;

use std::collections::HashMap;
use std::sync::Arc;

use andler_core::{
    BackendKind, HypervisorBackend, InstanceConfig,
    InstanceId, InstanceState,
};
use andler_qemu::QemuBackend;
use andler_store::Store;
use tokio::sync::RwLock;

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
    pub(crate) backends: HashMap<BackendKind, Arc<dyn HypervisorBackend>>,
    pub(crate) instances: RwLock<HashMap<InstanceId, InstanceRecord>>,
    pub(crate) store: Option<Store>,
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
    #[cfg(test)]
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

    pub(crate) fn backend_for(&self, kind: BackendKind) -> Result<&Arc<dyn HypervisorBackend>, DaemonError> {
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
    pub(crate) async fn persist_new_instance(&self, cfg: &InstanceConfig, state: &InstanceState) {
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
    /// Сохраняет обновлённую конфигурацию существующей записи в `store`
    /// (`andler edit`, см. `Daemon::update_instance_config`), если
    /// персистентность включена. Переиспользует `save_instance`
    /// (`INSERT OR REPLACE`) — тот же метод, что для новых инстансов;
    /// отдельного "update"-запроса к SQLite не нужно, upsert уже
    /// корректно перезаписывает существующую строку по `id`. См.
    /// документацию `persist_new_instance` про то, почему ошибка только
    /// логируется, не возвращается наружу.
    ///
    /// Также опportunistически обновляет `instance_dir/instance.toml`,
    /// если каталог реально существует (выводится из `cfg.disk.path`,
    /// см. `write_instance_toml` — тот же файл, что пишется при создании
    /// инстанса, см. PLAN.md, "5. Store config alongside instance"). Как
    /// и там, это инспекционная копия, не источник истины — SQLite
    /// остаётся им, поэтому неудача здесь тоже только логируется.
    pub(crate) async fn persist_config_update(&self, cfg: &InstanceConfig, state: &InstanceState) {
        if let Some(dir) = cfg.disk.path.parent() {
            types::write_instance_toml(dir, cfg).await;
        }

        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = store.save_instance(cfg, state).await {
            tracing::error!(
                instance_id = %cfg.id.0,
                error = %err,
                "failed to persist updated instance config to store"
            );
        }
    }

    /// Persist current state for an instance looked up by ID.
    pub(crate) async fn persist_state(&self, id: InstanceId, state: &InstanceState) {
        let cfg = {
            let instances = self.instances.read().await;
            match instances.get(&id) {
                Some(record) => record.config.clone(),
                None => return,
            }
        };
        self.persist_config_update(&cfg, state).await;
    }

}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
