//! Работа с дисками виртуальных машин через внешний процесс `qemu-img`.
//!
//! `qcow2` — низкоуровневые операции (create/clone/resize/compact/info).
//! `overlay` — доменно-осмысленный слой поверх `qcow2` специально для
//! overlay-дисков Android-инстансов (см. §4.4.2 архитектурного плана).
//! `error::DiskError` — общий тип ошибок обоих модулей.
//!
//! См. README.md этой папки для границ ответственности относительно
//! `andler-qemu`/`andler-core`.

pub mod error;
pub mod overlay;
pub mod qcow2;

pub use error::DiskError;
pub use overlay::{create_overlay, factory_reset, OverlayDisk};

