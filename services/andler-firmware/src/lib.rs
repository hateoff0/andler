//! Определение и провизионирование UEFI/OVMF firmware для ANDLER, а также
//! авто-детект остального железа/хоста (GPU, ARM-транслятор, аудио,
//! passt), нужного wizard'у для заполнения дефолтов Advanced-режима.
//!
//! ## Публичный API
//!
//! - [`detect_all()`] — основной вход для wizard'а: одним вызовом
//!   запускает все детекторы (OVMF, GPU, ARM, аудио, passt) и возвращает
//!   [`HardwareDefaults`]. См. `detect` модуль за деталями каждого шага.
//! - [`detect::detect_matched_pair()`] — низкоуровневый вход только для
//!   OVMF: ищет согласованную пару CODE+VARS (оба файла из одного
//!   пакета/дистрибутива). Это то, что нужно daemon'у при старте — daemon
//!   не запускает `detect_all()` целиком, ему не нужны GPU/ARM/audio.
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
    detect, detect_all, detect_matched_pair, provision_vars, reset_vars, AudioServer,
    DetectedOvmf, HardwareDefaults, KNOWN_OVMF_CODE_PATHS, KNOWN_OVMF_VARS_PATHS,
};
pub use error::FirmwareError;
