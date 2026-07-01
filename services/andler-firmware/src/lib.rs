//! Определение и провизионирование UEFI/OVMF firmware для ANDLER.
//!
//! Реализует автоматическую детекцию системных OVMF-файлов (логика
//! из `start.sh`/`start2.sh`, вынесенная в переиспользуемый крейт),
//! а также копирование шаблона `OVMF_VARS` в персональную копию инстанса.
//!
//! ## Публичный API
//!
//! - [`detect::detect_matched_pair()`] — основной вход: ищет согласованную
//!   пару CODE+VARS (оба файла из одного пакета/дистрибутива). Это то,
//!   что нужно вызвать daemon'у при старте или wizard'у при создании
//!   инстанса, когда пользователь не указал путь к OVMF явно.
//! - [`detect::detect()`] — независимый поиск CODE и VARS без требования
//!   принадлежности к одному пакету; `detect_matched_pair` деградирует
//!   до него как fallback.
//! - [`detect::provision_vars()`] — копирует шаблон VARS в инстанс
//!   (`tokio::fs::copy`, вызывается один раз при создании инстанса).
//! - [`detect::reset_vars()`] — удаляет и заново копирует VARS-шаблон
//!   (сброс EFI NVRAM), эквивалент `--reset-boot` из `start.sh`.
//! - [`detect::KNOWN_OVMF_CODE_PATHS`] / [`detect::KNOWN_OVMF_VARS_PATHS`]
//!   — константы известных путей, видимые для CLI (`--help` с перечнем
//!   проверяемых мест, подсказки wizard'а).
//! - [`error::FirmwareError`] — типизированные ошибки всего крейта.

pub mod detect;
pub mod error;

pub use detect::{
    detect, detect_matched_pair, provision_vars, reset_vars, DetectedOvmf,
    KNOWN_OVMF_CODE_PATHS, KNOWN_OVMF_VARS_PATHS,
};
pub use error::FirmwareError;
