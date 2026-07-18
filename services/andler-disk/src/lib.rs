//! Работа с дисками виртуальных машин через внешний процесс `qemu-img`.
//!
//! `qcow2` — низкоуровневые операции (create/clone/resize/compact/info).
//! `overlay` — доменно-осмысленный слой поверх `qcow2` специально для
//! overlay-дисков Android-инстансов (см. §4.4.2 архитектурного плана).
//! `clone` — три режима клонирования диска *существующего инстанса* в
//! новый диск (не от общего `base_image`, как `overlay` — см. модульную
//! документацию `clone` за тем, почему это разные операции).
//! `nbd` — общие утилиты для работы с NBD-устройствами и монтированием
//! разделов (переиспользуются `guest_tools`).
//! `guest_tools` — offline установка/удаление пакетов в гостевую ФС.
//! `diskspace` — pre-check свободного места перед операциями, которые
//! могут упасть посередине при нехватке места (снапшоты и т.п.).
//! `error::DiskError` — общий тип ошибок всех модулей.
//!
//! См. README.md этой папки для границ ответственности относительно
//! `andler-qemu`/`andler-core`.

pub mod arm_translator;
pub mod clone;
pub mod diskspace;
pub mod error;
pub mod guest_tools;
pub mod nbd;
pub mod overlay;
pub mod qcow2;
pub mod translator;
pub mod translator_download;

pub use clone::{full_standalone_clone, linked_clone, shared_base_clone, ClonedDisk};
pub use diskspace::check_available_space;
pub use error::DiskError;
pub use guest_tools::{GuestPackage, KNOWN_PACKAGES, PackageStatus};
pub use overlay::{create_overlay, factory_reset, OverlayDisk};
pub use qcow2::DiskInfo;
