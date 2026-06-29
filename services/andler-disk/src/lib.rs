//! Работа с дисками виртуальных машин через внешний процесс `qemu-img`.
//!
//! `qcow2` — низкоуровневые операции (create/clone/resize/compact/info).
//! `overlay` — доменно-осмысленный слой поверх `qcow2` специально для
//! overlay-дисков Android-инстансов (см. §4.4.2 архитектурного плана).
//! `clone` — три режима клонирования диска *существующего инстанса* в
//! новый диск (не от общего `base_image`, как `overlay` — см. модульную
//! документацию `clone` за тем, почему это разные операции).
//! `error::DiskError` — общий тип ошибок всех модулей.
//!
//! См. README.md этой папки для границ ответственности относительно
//! `andler-qemu`/`andler-core`.

pub mod clone;
pub mod error;
pub mod magisk;
pub mod overlay;
pub mod qcow2;

pub use clone::{full_standalone_clone, linked_clone, shared_base_clone, ClonedDisk};
pub use error::DiskError;
pub use overlay::{create_overlay, factory_reset, OverlayDisk};
pub use qcow2::DiskInfo;

