use std::path::PathBuf;

use andler_core::{BackendHandle, InstanceConfig, InstanceId, InstanceState};

/// Внутреннее состояние одного инстанса с точки зрения `Daemon`:
/// конфигурация (нужна, чтобы повторно резолвить `spawn`, и для будущего
/// сохранения в `andler-store`), текущее состояние FSM, и backend-хэндл —
/// есть только после первого успешного `start_instance` (до этого инстанс
/// существует как конфигурация, но ничего не запущено).
pub(crate) struct InstanceRecord {
    pub(crate) config: InstanceConfig,
    pub(crate) state: InstanceState,
    pub(crate) handle: Option<BackendHandle>,
}

/// Метаданные снапшота на уровне демона.
/// Сам снапшот (данные диска + состояние CPU/памяти) хранится внутри
/// qcow2-файла — здесь только метаданные для быстрого доступа.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub id: uuid::Uuid,
    pub instance_id: InstanceId,
    pub tag: String,
    pub description: Option<String>,
    pub created_at: String,
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
pub(crate) struct InstanceDirGuard {
    pub(crate) path: PathBuf,
    armed: bool,
}

impl InstanceDirGuard {
    pub fn new(path: PathBuf) -> Self {
        InstanceDirGuard { path, armed: true }
    }

    /// Снимает страховку — вызывается после того, как `instance_dir`
    /// больше не нужно удалять (вся последовательность создания
    /// инстанса завершилась успешно, включая регистрацию в `Daemon`).
    pub fn disarm(&mut self) {
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

/// Пишет `<instance_dir>/instance.toml` — человекочитаемая копия
/// `InstanceConfig`, лежащая рядом с диском/`VARS.fd` самого инстанса
/// (см. PLAN.md, item 5, "Store config alongside instance, not in DB").
/// Вызывается из `create_android_instance`/`create_linux_instance` (не
/// из общего `create_instance`!) — именно эти две функции сами создают
/// и владеют `instance_dir` под `instances_root`; универсальный
/// `create_instance` принимает уже готовый `InstanceConfig` откуда
/// угодно (тесты, `LinuxVm` с произвольным пользовательским
/// `disk.path`), и не может считать `disk.path.parent()` "своим"
/// каталогом — писать туда `instance.toml` из общего пути означало бы
/// разбрасывать файлы по каталогам, не связанным с этим инстансом.
///
/// Реализована пока только запись — SQLite остаётся единственным
/// источником истины при `Daemon::restore` (чтение `instance.toml` в
/// приоритете над SQLite, миграция legacy-инстансов без файла и
/// TOCTOU-обработка одновременного редактирования пользователем и
/// демоном — из полного предложения в PLAN.md не реализованы). Файл
/// уже сейчас можно открыть текстовым редактором/`cat`/`jq` для осмотра
/// конфигурации без обращения к демону — то есть часть непосредственной
/// ценности предложения (инспекция, бэкап каталога) уже есть, часть
/// (редактирование конфигурации в обход демона с последующим подхватом
/// изменений) — нет.
///
/// Ошибка записи только логируется, не возвращается наружу — тот же
/// принцип, что и у `Daemon::persist_new_instance`: `instance.toml` не
/// является источником истины на данный момент (им остаётся SQLite),
/// поэтому его временная недоступность (диск полон, права) не должна
/// мешать уже совершившемуся созданию инстанса.
pub(crate) async fn write_instance_toml(instance_dir: &std::path::Path, cfg: &InstanceConfig) {
    let toml_string = match toml::to_string_pretty(cfg) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(
                instance_id = %cfg.id.0,
                error = %err,
                "failed to serialize instance.toml (instance was still created successfully)"
            );
            return;
        }
    };

    let path = instance_dir.join("instance.toml");
    if let Err(err) = tokio::fs::write(&path, toml_string).await {
        tracing::error!(
            instance_id = %cfg.id.0,
            path = %path.display(),
            error = %err,
            "failed to write instance.toml (instance was still created successfully)"
        );
    }
}

/// Удаляет файлы инстанса, принадлежащие только ему (диск, персональная
/// копия OVMF_VARS), а затем удаляет и сам родительский каталог: если
/// это собственный каталог инстанса (`<instances_root>/<id>/`) — целиком
/// и рекурсивно (там могло остаться что угодно ещё, например
/// `qemu.log`), если это чужой/пользовательский путь — только если он
/// уже пуст. Используется только из `Daemon::remove_instance` при
/// `purge: true` — см. документацию там за полным обоснованием того, что
/// удаляется и почему.
///
/// Каждая ошибка удаления логируется отдельно, не агрегируется и не
/// возвращается — вызывающая сторона (`remove_instance`) уже считает
/// purge best-effort шагом после того, как сама запись инстанса уже
/// удалена; частичный сбой здесь не должен превращаться в `Err` для уже
/// выполненной операции удаления.
pub(crate) async fn purge_instance_files(id: InstanceId, config: &InstanceConfig) {
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

    // `remove_dir` only succeeds on an already-empty directory — that
    // used to silently leave the instance directory behind for any
    // instance with more than the two files above in it (e.g. Linux
    // instances with an extra file next to the disk, or any instance at
    // all once `qemu.log` started being written into this same
    // directory — see `andler_qemu::process::QemuProcess::spawn`). See
    // PLAN.md, item 2, "--purge remove does not delete instance
    // folder".
    //
    // `remove_dir_all` is only safe to use unconditionally if we're sure
    // `parent` really is *this instance's own* directory and not some
    // unrelated, possibly user-supplied path that merely happens to be
    // the parent of `disk.path` (a `LinuxVm` can point `disk.path`
    // anywhere the user chose — see `create_instance`). Instance
    // directories are always named after the instance's own UUID (see
    // `instance_ops.rs`/`clone_ops.rs`: `instances_root.join(id.0.to_string())`),
    // so comparing the directory's name against `id.0.to_string()`
    // (rather than a generic "looks like some UUID" shape check) is both
    // precise and cheap — no need to also verify `parent` sits under
    // `instances_root`, since a name collision with a real instance's
    // own id is not something a user-supplied `disk.path` could produce
    // by accident.
    if let Some(parent) = config.disk.path.parent() {
        let is_instance_dir = parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == id.0.to_string());

        if is_instance_dir {
            if let Err(err) = tokio::fs::remove_dir_all(parent).await {
                tracing::error!(
                    instance_id = %id.0,
                    path = %parent.display(),
                    error = %err,
                    "purge: failed to remove instance directory"
                );
            }
        } else {
            // Not our own instance directory (e.g. a `LinuxVm` with a
            // user-chosen `disk.path` elsewhere) — best-effort, only
            // removes it if it's already empty, never recursively: we
            // have no way to know what else might legitimately live
            // there.
            let _ = tokio::fs::remove_dir(parent).await;
        }
    }
}

/// Краткая сводка по инстансу для `list_instances` (без полной
/// конфигурации, которая в большинстве вызовов не нужна).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
}
