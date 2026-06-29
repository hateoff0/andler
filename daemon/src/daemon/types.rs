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

/// Краткая сводка по инстансу для `list_instances` (без полной
/// конфигурации, которая в большинстве вызовов не нужна).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: InstanceId,
    pub name: String,
    pub state: InstanceState,
}
