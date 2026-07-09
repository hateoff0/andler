//! Конфигурация диска инстанса.
//!
//! Источник истины — исходная референсная конфигурация (ранее описанная в
//! `scripts/start.sh`, который был удалён после миграции всей логики в Rust):
//! `-drive file=disk.qcow2,format=qcow2,if=none,discard=on,detect-zeroes=on,aio=threads`
//! + `-device virtio-blk-pci,...,num-queues=4`. Для Android-инстансов —
//! `base_image`/overlay-механизм из §4.4.2
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md.
//! Дефолтный размер диска у `reference_default` — **256 GiB**, не 40 GiB
//! из исходной референсной конфигурации. См. PLAN.md, раздел «Disk management»: единый дефолт
//! 256 GiB (вместо более раннего варианта с раздельными 64/128 GiB по
//! типу гостя) выбран осознанно как номинальный верхний предел —
//! благодаря thin provisioning у qcow2 он не означает немедленно занятое
//! место на хосте. 40 GiB из исходной референсной конфигурации остаётся исторически
//! зафиксированным как нижняя граница, которой реальный размер
//! современной Linux/Android-инсталляции после установки обычно не
//! превышает, но не как актуальный дефолт ANDLER.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Формат файла диска.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskFormat {
    Qcow2,
    Raw,
    Vdi,
}

/// Конфигурация диска инстанса.
///
/// Для `InstanceKind::LinuxVm` обычно `base_image: None` — диск создаётся
/// с нуля через `andler-disk::qcow2::create`. Для `InstanceKind::AndroidVm`
/// `base_image` указывает на закэшированный/скачанный базовый образ
/// (см. §4.4.2 архитектурного плана), а `path` — на overlay-диск конкретного
/// инстанса с `backing_file = base_image`; базовый образ при этом никогда
/// не модифицируется напрямую (`andler-disk::overlay`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskConfig {
    /// Путь к файлу диска, который реально подключается к QEMU
    /// (`-drive file=...`). Для Android-инстанса это overlay, а не
    /// базовый образ.
    pub path: PathBuf,
    /// Логический размер диска в байтах (виртуальный размер, не
    /// фактический занятый объём на хосте — для thin-provisioned дисков
    /// это разные числа).
    pub size_bytes: u64,
    pub format: DiskFormat,
    /// Базовый образ (`backing_file`) для linked-clone/overlay-дисков.
    /// `None` — обычный самостоятельный диск без backing chain.
    pub base_image: Option<PathBuf>,
    /// Растить файл по мере фактического использования (`qemu-img create`
    /// без предварительной полной аллокации). В исходной референсной конфигурации
    /// это подразумевалось неявно самим форматом `qcow2`.
    pub thin_provisioning: bool,
    /// Выполнять TRIM/discard при штатном завершении инстанса —
    /// освобождает место на хосте от удалённых внутри гостя данных.
    /// Соответствует `discard=on,detect-zeroes=on` в исходной референсной конфигурации,
    /// хотя там это включено постоянно при работе диска, а не только при shutdown;
    /// разделение на runtime-discard и shutdown-trim — решение, которое
    /// `andler-qemu` принимает на основе этого флага плюс самого факта
    /// `discard=on` устройства (см. TODO в `andler-qemu` README).
    pub trim_on_shutdown: bool,
    /// Автоматически компактифицировать (`qemu-img convert` во временный
    /// файл + atomic rename — см. `andler_disk::qcow2::compact`) диск
    /// сразу после штатной остановки инстанса.
    ///
    /// **Выключено по умолчанию** (`false`) — пользователь должен явно
    /// включить эту опцию (CLI-флаг при создании, `andler config <id>
    /// --set compact-on-shutdown=true`, или TOML-поле), не наоборот.
    /// Причина не делать это дефолтом для всех qcow2-дисков (а они и есть
    /// дефолт ANDLER, см. `reference_default`): compact — потенциально
    /// длительная операция (полная перезапись файла диска через `qemu-img
    /// convert`, время растёт с фактическим занятым местом на хосте), и
    /// превращение каждого `andler stop` в операцию, которая может занять
    /// заметное время и нагрузить диск хоста на запись, было бы
    /// неожиданным побочным эффектом без явного согласия пользователя.
    ///
    /// Применимо только к `DiskFormat::Qcow2` — у raw нет qcow2-метаданных
    /// для компактификации (см. `andler_disk::qcow2::compact` и раздел
    /// «Disk management» в PLAN.md); для не-qcow2 дисков значение этого
    /// поля просто игнорируется на уровне backend'а, не является ошибкой
    /// конфигурации сама по себе (диск мог быть переключён в raw уже
    /// после того, как флаг был включён).
    pub compact_on_shutdown: bool,
    /// Таймаут ожидания завершения async job (snapshot-save/load/delete)
    /// в секундах. `None` — 30 секунд по умолчанию. Увеличьте для
    /// очень больших дисков (сотни GiB), где snapshot-операции занимают
    /// больше времени.
    pub snapshot_timeout_secs: Option<u64>,
}

impl DiskConfig {
    pub const GIB: u64 = 1024 * 1024 * 1024;

    /// Конфигурация обычного самостоятельного диска (`LinuxVm`):
    /// `disk.qcow2`, **256 GiB** (номинальный верхний предел, не сразу
    /// занятое место на хосте — см. doc-комментарий модуля), qcow2,
    /// сознательно отличается от 40 GiB в исходной референсной конфигурации — см.
    /// PLAN.md, раздел «Disk management» и блок «Что изменилось и
    /// почему» при нём.
    pub fn reference_default(path: PathBuf) -> Self {
        DiskConfig {
            path,
            size_bytes: 256 * Self::GIB,
            format: DiskFormat::Qcow2,
            base_image: None,
            thin_provisioning: true,
            trim_on_shutdown: true,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        }
    }

    /// Конфигурация overlay-диска для Android-инстанса: тот же набор
    /// флагов, что у `reference_default`, но с явно заданным
    /// `base_image` — overlay всегда thin-provisioned и qcow2, так как
    /// backing-chain в QEMU поддерживается только для qcow2.
    /// См. §4.4.2 архитектурного плана.
    pub fn overlay(path: PathBuf, base_image: PathBuf, size_bytes: u64) -> Self {
        DiskConfig {
            path,
            size_bytes,
            format: DiskFormat::Qcow2,
            base_image: Some(base_image),
            thin_provisioning: true,
            trim_on_shutdown: true,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_is_256_gib_thin_provisioned_qcow2() {
        // Имя теста ранее было `reference_default_matches_start_sh` —
        // переименовано, т.к. дефолт теперь намеренно *не* совпадает со
        // исходной референсной конфигурацией (40 GiB). См. PLAN.md, раздел
        // «Disk management»: не "исправляйте" это обратно на 40 без сверки с
        // планом — это было умышленное изменение, а не регрессия.
        let cfg = DiskConfig::reference_default(PathBuf::from("disk.qcow2"));
        assert_eq!(cfg.size_bytes, 256 * DiskConfig::GIB);
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert_eq!(cfg.base_image, None);
        assert!(cfg.thin_provisioning);
        assert!(cfg.trim_on_shutdown);
        assert!(
            !cfg.compact_on_shutdown,
            "compact_on_shutdown must be opt-in, not a default-on behavior — see PLAN.md"
        );
    }

    #[test]
    fn overlay_points_at_base_image() {
        let base = PathBuf::from("/var/lib/andler/images/android-13-gapps.qcow2");
        let cfg = DiskConfig::overlay(
            PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2"),
            base.clone(),
            20 * DiskConfig::GIB,
        );
        assert_eq!(cfg.base_image, Some(base));
        assert_eq!(cfg.format, DiskFormat::Qcow2);
        assert!(!cfg.compact_on_shutdown);
    }
}
