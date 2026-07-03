use std::path::PathBuf;

use super::Daemon;
use super::error::DaemonError;
use super::types::{InstanceDirGuard, InstanceRecord};
use andler_core::{
    BackendError, DiskFormat, InstanceConfig, InstanceEvent, InstanceId,
    InstanceState,
};

impl Daemon {
    /// Resolves a user-supplied instance reference to a concrete
    /// registered [`InstanceId`] — either a full UUID (fast path, exact
    /// match, no lock needed) or a unique prefix of one, Docker-style
    /// (`andler status a1b2c3` instead of the full UUID). See PLAN.md,
    /// "Partial instance ID".
    ///
    /// The prefix is matched against the standard hyphenated lowercase
    /// string form of each registered `InstanceId` (the same form shown
    /// to users by `andler status`/`list_instances`), case-insensitively,
    /// so a copy-pasted prefix from `andler status` output always works
    /// regardless of case.
    ///
    /// - Zero matches -> [`DaemonError::InstanceRefNotFound`] (distinct
    ///   from [`DaemonError::InstanceNotFound`], which is for a
    ///   well-formed but unregistered full `InstanceId`).
    /// - Exactly one match -> `Ok`.
    /// - More than one match -> [`DaemonError::AmbiguousInstanceId`],
    ///   carrying every matching `InstanceId` so the caller can show the
    ///   user exactly what to disambiguate between, instead of forcing a
    ///   separate `andler status` round-trip.
    ///
    /// A syntactically full, valid UUID is always treated as a full ID,
    /// never as a "prefix that happens to match nothing else" — it does
    /// not need to be registered yet for this function to accept it (the
    /// caller decides whether an unregistered-but-well-formed ID is an
    /// error, same as before this method existed).
    pub async fn resolve_instance_id(&self, raw: &str) -> Result<InstanceId, DaemonError> {
        if raw.is_empty() {
            return Err(DaemonError::EmptyInstanceRef);
        }

        if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
            return Ok(InstanceId(uuid));
        }

        let needle = raw.to_ascii_lowercase();
        let instances = self.instances.read().await;
        let matches: Vec<InstanceId> = instances
            .keys()
            .filter(|id| id.0.to_string().starts_with(&needle))
            .copied()
            .collect();

        match matches.len() {
            0 => Err(DaemonError::InstanceRefNotFound(raw.to_string())),
            1 => Ok(matches[0]),
            _ => Err(DaemonError::AmbiguousInstanceId {
                prefix: raw.to_string(),
                candidates: matches,
            }),
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
    /// и персональной копией `OVMF_VARS`, и только потом передаёт получившийся
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
        profile: andler_core::AndroidProfile,
        instance_name: String,
        base_image_path: PathBuf,
        instances_root: PathBuf,
        overlay_size_bytes: u64,
        ovmf_vars_template: PathBuf,
        magisk_dir: Option<PathBuf>,
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
        andler_firmware::provision_vars(&ovmf_vars_template, &ovmf_vars_path)
            .await
            .map_err(|e| DaemonError::Firmware(e.to_string()))?;

        let overlay = andler_disk::overlay::create_overlay(
            &instance_dir,
            &base_image_path,
            overlay_size_bytes,
        )
        .await?;

        // Magisk provisioning — offline-установка root-доступа в overlay
        // перед первым запуском. Выполняется до регистрации инстанса, чтобы
        // при ошибке provisioning instance_dir был автоматически очищен
        // через dir_guard (инстанс не остаётся в битом состоянии).
        if profile.root == andler_core::RootMode::Magisk {
            let magisk_dir = magisk_dir.ok_or_else(|| DaemonError::Disk(
                andler_disk::DiskError::NbdSetupFailed(
                    "--magisk-dir is required when root=magisk".to_string(),
                ),
            ))?;
            andler_disk::magisk::provision_magisk(
                &overlay.overlay_path,
                &magisk_dir,
            ).await?;
        }

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
    ///
    /// Если у инстанса включён `DiskConfig::compact_on_shutdown` и формат
    /// диска — qcow2, после успешной остановки в фоне (без блокировки
    /// возврата из этого метода — см. `spawn_compact_on_shutdown`)
    /// запускается `andler_disk::qcow2::compact`. Выключено по умолчанию
    /// — см. PLAN.md, раздел «Disk management», и doc-комментарий самого
    /// поля в `andler_core::config::disk::DiskConfig`.
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
        let disk = record.config.disk.clone();
        drop(instances);

        self.persist_state(id, &final_state).await;

        if result.is_ok() {
            spawn_compact_on_shutdown(id, disk);
        }

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
                super::types::purge_instance_files(id, &config).await;
            }
        }

        Ok(())
    }
}

/// Запускает компактификацию диска инстанса в фоне (`tokio::spawn`), если
/// для него включён `DiskConfig::compact_on_shutdown` и формат диска —
/// qcow2 — единственный формат с qcow2-метаданными, которые вообще можно
/// компактифицировать (см. `andler_disk::qcow2::compact` и PLAN.md,
/// раздел «Disk management»).
///
/// Намеренно **не** блокирует `stop_instance` — компактификация (полная
/// перезапись файла диска через `qemu-img convert`) может занимать
/// заметное время на больших дисках, и пользователь, вызвавший `andler
/// stop`, не должен ждать её завершения, чтобы получить управление
/// обратно. Ошибки логируются через `tracing::error!`, а не
/// пробрасываются никуда дальше — на этом этапе нет канала, через
/// который асинхронная пост-shutdown задача могла бы сообщить о неудаче
/// вызывающей стороне `stop_instance` (она уже получила `Ok(())` к этому
/// моменту). См. также `andler logs`/будущий `andler metrics` как место,
/// где такой статус мог бы стать видимым пользователю, если на практике
/// окажется, что молчаливый лог недостаточен.
///
/// Не проверяет текущее состояние инстанса (`Stopped` vs что-то ещё) —
/// вызывается только из `stop_instance` сразу после успешного перехода в
/// `Stopped`, так что повторная проверка состояния была бы избыточной
/// гонкой с самим собой, а не дополнительной защитой.
fn spawn_compact_on_shutdown(id: InstanceId, disk: andler_core::DiskConfig) {
    if !disk.compact_on_shutdown {
        return;
    }
    if disk.format != DiskFormat::Qcow2 {
        tracing::debug!(
            instance_id = %id.0,
            format = ?disk.format,
            "compact_on_shutdown is enabled but disk format is not qcow2 — skipping, \
             see PLAN.md, раздел «Disk management»"
        );
        return;
    }

    let path = disk.path.clone();
    tokio::spawn(async move {
        tracing::info!(
            instance_id = %id.0,
            path = %path.display(),
            "compact_on_shutdown: starting automatic disk compaction"
        );
        match andler_disk::qcow2::compact(&path).await {
            Ok(()) => {
                tracing::info!(
                    instance_id = %id.0,
                    path = %path.display(),
                    "compact_on_shutdown: disk compaction finished"
                );
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %id.0,
                    path = %path.display(),
                    error = %err,
                    "compact_on_shutdown: automatic disk compaction failed"
                );
            }
        }
    });
}
