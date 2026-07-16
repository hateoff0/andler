use std::path::PathBuf;

use super::Daemon;
use super::error::DaemonError;
use super::types::{InstanceDirGuard, InstanceRecord, write_instance_toml};
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

        // Validate that the input looks like a hex prefix (only [0-9a-fA-F-] allowed).
        // This distinguishes "malformed input" from "valid prefix, no match" — the former
        // maps to InvalidArgument in gRPC, the latter to NotFound.
        if !raw.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(DaemonError::MalformedInstanceRef(raw.to_string()));
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

        // `ensure_private_dir` sets `0700`, not just the default umask —
        // the instance directory holds the disk image, OVMF_VARS (may
        // contain Secure Boot keys), instance.toml, and qemu.log; none
        // of that should be readable by other local users by default.
        // See PLAN.md, item 20b, "No file permission controls".
        andler_core::paths::ensure_private_dir(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        // Android requires UEFI — reject creation without OVMF template.
        if ovmf_vars_template.as_os_str().is_empty() {
            return Err(DaemonError::Firmware(
                "Android requires UEFI/OVMF. Provide an OVMF_VARS template.".to_string(),
            ));
        }

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

        write_instance_toml(&instance_dir, &cfg).await;

        let registered_id = self.create_instance(cfg).await?;
        // Вся последовательность (каталог, OVMF_VARS, overlay,
        // регистрация в Daemon) успешна — instance_dir больше не
        // подлежит автоматической очистке через Drop этого guard'а.
        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Создаёт Linux-инстанс: каталог, OVMF VARS (если шаблон найден),
    /// qcow2-диск и регистрирует инстанс. Аналог `create_android_instance`
    /// для Linux — тот же паттерн: ресурсы создаются до регистрации, при
    /// ошибке `InstanceDirGuard` удаляет частично созданные файлы.
    pub async fn create_linux_instance(
        &self,
        mut cfg: InstanceConfig,
        instances_root: PathBuf,
        ovmf_vars_template: PathBuf,
    ) -> Result<InstanceId, DaemonError> {
        let id = InstanceId::new();
        let instance_dir = instances_root.join(id.0.to_string());
        let mut dir_guard = InstanceDirGuard::new(instance_dir.clone());

        // `ensure_private_dir` sets `0700`, not just the default umask —
        // the instance directory holds the disk image, OVMF_VARS (may
        // contain Secure Boot keys), instance.toml, and qemu.log; none
        // of that should be readable by other local users by default.
        // See PLAN.md, item 20b, "No file permission controls".
        andler_core::paths::ensure_private_dir(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        if !ovmf_vars_template.as_os_str().is_empty() {
            let ovmf_vars_path = instance_dir.join("VARS.fd");
            andler_firmware::provision_vars(&ovmf_vars_template, &ovmf_vars_path)
                .await
                .map_err(|e| DaemonError::Firmware(e.to_string()))?;
            cfg.firmware.ovmf_vars_path = ovmf_vars_path;
        }

        if cfg.disk.format == DiskFormat::Qcow2 && !cfg.disk.path.exists() {
            // Anchor a freshly-created disk inside our own instance_dir,
            // the same way `ovmf_vars_path` above always is — matches
            // `create_android_instance`'s overlay (also always inside
            // `instance_dir`) and the documented
            // `<instances_root>/<uuid>/{VARS.fd,disk.qcow2}` layout (see
            // PLAN.md, item 5's storage table). Before this fix,
            // whatever path the caller put in `cfg.disk.path` (e.g. the
            // wizard's `<instances_root>/<name>-disk.qcow2`, flat, not
            // nested under a per-instance directory) was used as-is —
            // meaning disk.qcow2 and VARS.fd for the same instance ended
            // up in two different directories, and anything keyed off
            // `disk.path.parent()` as "the instance's own directory"
            // (qemu.log — see `QemuProcess::spawn` — and purge's
            // recursive-delete decision in `purge_instance_files`) was
            // silently wrong for every Linux VM.
            //
            // Only for a disk that doesn't exist yet: an *existing*
            // disk the caller points at (e.g. a hand-written TOML/CLI
            // config referencing a disk the user manages themselves
            // elsewhere on the filesystem) is left exactly where it is
            // — this is the same "not our own directory" case already
            // documented in `purge_instance_files`, we must not move a
            // file the user didn't ask us to move.
            let disk_file_name = cfg
                .disk
                .path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("disk.qcow2"));
            cfg.disk.path = instance_dir.join(disk_file_name);

            andler_disk::qcow2::create(&cfg.disk.path, cfg.disk.size_bytes)
                .await
                .map_err(DaemonError::Disk)?;
        }

        cfg.id = id;

        write_instance_toml(&instance_dir, &cfg).await;

        let registered_id = self.create_instance(cfg).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Запускает инстанс: `Created -> Starting -> Running`, либо —
    /// перезапускает уже запускавшийся: `Stopped`/`Error -> Starting ->
    /// Running` тем же путём (см. `andler_core::fsm` — обе группы
    /// исходных состояний ведут в `Starting` одним и тем же `Start`).
    /// Ничего специфичного для "первого запуска" здесь нет и не было —
    /// весь метод ниже одинаково валиден для повторного запуска уже
    /// существующего `InstanceConfig`, ограничение раньше было только в
    /// самой FSM. При ошибке backend'а переводит запись в
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
        // Extract what's needed and release the lock before the QMP
        // round-trip below — holding a read lock across `.await` would
        // serialize every other operation on `self.instances` (create,
        // remove, status queries, ...) behind however long `pause`
        // takes to actually talk to QEMU. Same fix, same rationale, as
        // `resume_instance` right below and as `start_instance`/
        // `stop_instance` already do above. See PLAN.md, item 21b,
        // "RwLock held across await in pause/resume".
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let handle = record.handle.clone().ok_or_else(|| {
                DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string()))
            })?;
            let backend = self.backend_for(record.config.backend)?.clone();

            (backend, handle)
        };

        backend.pause(&handle).await.map_err(DaemonError::Backend)
    }

    /// Возобновляет приостановленный инстанс. См. документацию
    /// `pause_instance` про то, почему `InstanceState` записи демона не
    /// меняется здесь напрямую.
    pub async fn resume_instance(&self, id: InstanceId) -> Result<(), DaemonError> {
        let (backend, handle) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;

            let handle = record.handle.clone().ok_or_else(|| {
                DaemonError::Backend(BackendError::HandleNotFound(id.0.to_string()))
            })?;
            let backend = self.backend_for(record.config.backend)?.clone();

            (backend, handle)
        };

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

    /// Устанавливает пакет в гостевую ОС инстанса.
    ///
    /// Auto-fallback логика:
    /// 1. Если VM запущена (Running/Paused) и guest agent доступен → online
    ///    через guest-exec (QEMU Guest Agent).
    /// 2. Если VM остановлена (Created/Stopped) → offline через qemu-nbd + mount.
    /// 3. Если VM запущена, но guest agent недоступен → ошибка с инструкцией
    ///    остановить VM для offline установки.
    pub async fn install_guest_agent(
        &self,
        id: InstanceId,
        package: String,
    ) -> Result<(), DaemonError> {
        let (state, disk_path, handle, backend_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&handle).await? {
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: format!(
                            "VM is running but guest agent is not available; \
                             stop the VM first to install `{package}` offline"
                        ),
                    });
                }

                backend.guest_exec_install(&handle, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
                    package = %package,
                    "package installed via online guest-exec"
                );
                Ok(())
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                andler_disk::guest_tools::install_agent_offline(&disk_path, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
                    package = %package,
                    "package installed via offline qemu-nbd"
                );
                Ok(())
            }
            other => Err(DaemonError::GuestAgentUnavailable {
                instance_id: id,
                message: format!(
                    "instance is in state {other:?}; must be Running/Paused (online) \
                     or Created/Stopped (offline)"
                ),
            }),
        }
    }

    /// Удаляет пакет из гостевой ОС инстанса.
    ///
    /// Та же auto-fallback логика, что у `install_guest_agent`.
    pub async fn remove_guest_agent(
        &self,
        id: InstanceId,
        package: String,
    ) -> Result<(), DaemonError> {
        let (state, disk_path, handle, backend_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
            )
        };

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                if !backend.is_guest_agent_available(&handle).await? {
                    return Err(DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: format!(
                            "VM is running but guest agent is not available; \
                             stop the VM first to remove `{package}` offline"
                        ),
                    });
                }

                backend.guest_exec_remove(&handle, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
                    package = %package,
                    "package removed via online guest-exec"
                );
                Ok(())
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                andler_disk::guest_tools::remove_agent_offline(&disk_path, &package).await?;
                tracing::info!(
                    instance_id = %id.0,
                    package = %package,
                    "package removed via offline qemu-nbd"
                );
                Ok(())
            }
            other => Err(DaemonError::GuestAgentUnavailable {
                instance_id: id,
                message: format!(
                    "instance is in state {other:?}; must be Running/Paused (online) \
                     or Created/Stopped (offline)"
                ),
            }),
        }
    }

    /// Возвращает списокKNOWN_PACKAGES со статусом в гостевой ОС.
    ///
    /// Auto-fallback: online (guest-exec) если VM запущена, offline
    /// (qemu-nbd) если остановлена.
    pub async fn list_guest_packages(
        &self,
        id: InstanceId,
    ) -> Result<Vec<(String, String, String)>, DaemonError> {
        let (state, disk_path, handle, backend_kind) = {
            let instances = self.instances.read().await;
            let record = instances
                .get(&id)
                .ok_or(DaemonError::InstanceNotFound(id))?;
            (
                record.state.clone(),
                record.config.disk.path.clone(),
                record.handle.clone(),
                record.config.backend,
            )
        };

        let mut results = Vec::new();

        match &state {
            InstanceState::Running | InstanceState::Paused => {
                let handle = handle.ok_or_else(|| {
                    DaemonError::GuestAgentUnavailable {
                        instance_id: id,
                        message: "instance is running but has no backend handle".to_string(),
                    }
                })?;

                let backend = self.backend_for(backend_kind)?;

                for pkg in andler_disk::guest_tools::KNOWN_PACKAGES {
                    let installed = backend
                        .guest_check_binary_installed(&handle, pkg.binary_check)
                        .await
                        .unwrap_or(false);
                    results.push((
                        pkg.name.to_string(),
                        pkg.description.to_string(),
                        if installed { "installed" } else { "not_installed" }.to_string(),
                    ));
                }
            }
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. } => {
                if !disk_path.exists() {
                    return Err(DaemonError::InstanceNotFound(id));
                }

                // Offline: mount and check
                let package_status = andler_disk::guest_tools::check_all_packages_offline_with_disk(&disk_path)?;
                for (pkg, status) in package_status {
                    results.push((
                        pkg.name.to_string(),
                        pkg.description.to_string(),
                        match status {
                            andler_disk::guest_tools::PackageStatus::Installed => "installed",
                            andler_disk::guest_tools::PackageStatus::NotInstalled => "not_installed",
                            andler_disk::guest_tools::PackageStatus::Unknown => "unknown",
                        }.to_string(),
                    ));
                }
            }
            other => {
                return Err(DaemonError::GuestAgentUnavailable {
                    instance_id: id,
                    message: format!(
                        "instance is in state {other:?}; cannot list packages"
                    ),
                });
            }
        }

        Ok(results)
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
