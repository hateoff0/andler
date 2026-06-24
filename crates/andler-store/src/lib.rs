//! Персистентность `InstanceConfig`/`InstanceState` на sqlite.
//!
//! Этот крейт не содержит бизнес-логики переходов состояний — только
//! CRUD над тем, что уже решено в `andler-core::fsm`/`andler-daemon`. См.
//! README этой папки и `docs/architecture/CORE_ARCHITECTURE_PLAN.md`.

pub mod error;
pub mod store;

pub use error::StoreError;
pub use store::{Store, StoredInstance};
