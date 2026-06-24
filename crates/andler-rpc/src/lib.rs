//! `andler-rpc` — единая точка правды по протоколу `andlerd` <-> `andler`
//! (CLI)/GUI: `proto/andler.proto`, скомпилированный в Rust через
//! `tonic-build`/`prost` (см. `build.rs`), плюс конвертации между
//! сгенерированными типами и доменными типами `andler-core`.
//!
//! Конвертации живут здесь, а не в `andler-daemon`/`andler-cli` —
//! `andler-rpc` уже зависит от обоих (proto-типы и `andler-core`), и обе
//! стороны протокола (сервер в `andler-daemon`, клиент в `andler-cli`)
//! используют один и тот же код преобразования, а не пишут его дважды.
//!
//! См. README.md этого крейта про то, почему набор rpc-методов в
//! `andler.proto` — намеренное подмножество §5.1 архитектурного плана.

pub mod proto {
    tonic::include_proto!("andler");
}

pub mod convert;

pub use convert::ConvertError;
