//! Определение и провизионирование UEFI/OVMF firmware для ANDLER, а также
//! авто-детект остального железа/хоста (GPU, ARM-транслятор, аудио,
//! passt) и сбор GPU-метрик — всё, что живёт на уровне хоста, а не
//! отдельной VM.
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
//!   (сброс EFI NVRAM), эквивалент `--reset-boot` из исходной референсной
//!   конфигурации.
//! - [`detect::KNOWN_OVMF_CODE_PATHS`] / [`detect::KNOWN_OVMF_VARS_PATHS`]
//!   — константы известных путей, видимые для CLI (`--help` с перечнем
//!   проверяемых мест, подсказки wizard'а).
//! - [`metrics::read_gpu_metrics()`] / [`metrics::merge_gpu_metrics()`] —
//!   GPU-метрики хоста (VRAM, load %). Переехали сюда из
//!   `backends/andler-qemu` — детекция и мониторинг GPU читают одни и те
//!   же sysfs-пути/vendor-тулы, логично держать их в одном месте.
//!   Per-VM метрики (CPU%, RAM, disk/network I/O — всё, что требует PID)
//!   остаются в `backends/andler-qemu::metrics`, который вызывает эти две
//!   функции только для GPU-полей.
//! - [`error::FirmwareError`] — типизированные ошибки всего крейта.

pub mod detect;
pub mod error;
pub mod metrics;

pub use detect::{
    detect, detect_all, detect_matched_pair, provision_vars, reset_vars, AudioServer,
    DetectedOvmf, HardwareDefaults, KNOWN_OVMF_CODE_PATHS, KNOWN_OVMF_VARS_PATHS,
};
pub use error::FirmwareError;
