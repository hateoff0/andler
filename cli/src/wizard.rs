//! Интерактивный wizard создания VM через `inquire` (стрелки/Enter/Space).
//!
//! ## Три пути создания VM
//!
//! Все три режима доступны параллельно, wizard не заменяет остальные:
//! - **TOML** (`andler create --file vm.toml`) — воспроизводимые конфиги
//! - **CLI** (`andler create --kind linux --name ... --iso-path ...`) — знаю все параметры
//! - **Wizard** (`andler create`) — интерактивный опрос, этот файл
//!
//! ## Когда запускается
//!
//! Wizard запускается из `create::handle`, когда `--file` не передан и
//! обязательные CLI-параметры (`--kind`, `--name`, специфичные для типа)
//! отсутствуют. Если TTY недоступен — `inquire` возвращает `InquireError::NotTTY`,
//! и мы выводим понятную ошибку вместо сырого panic'а библиотеки.
//!
//! ## Basic / Advanced
//!
//! - **Basic** (дефолт) — спрашивает только: тип, имя, ISO/base-image,
//!   размер диска. Всё остальное берётся из авто-детектированных дефолтов.
//! - **Advanced** (`andler create --advanced`) — полный список вопросов
//!   по всем категориям.
//!
//! ## Итоговая сводка
//!
//! Перед `create_instance` wizard показывает полную сводку всех выбранных
//! (и дефолтных) значений и просит подтверждение.

use std::path::PathBuf;

use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};
use inquire::{Confirm, CustomType, InquireError, Select, Text};

use crate::helpers::ensure_qcow2_extension;
use crate::{CliAndroidVersion, CliRootMode};

/// Результат wizard — то, что нужно передать в gRPC.
pub enum WizardResult {
    Linux(CreateInstanceRequest, String /* instances_root */),
    Android(CreateAndroidInstanceRequest),
}

/// Основная точка входа wizard'а.
///
/// `partial` — уже переданные через CLI флаги (могут быть `None`).
/// Wizard заполняет только то, чего не хватает.
pub async fn run(partial: PartialArgs, advanced: bool) -> Result<WizardResult, WizardError> {
    // Проверяем TTY до первого вопроса — иначе получим ошибку посередине.
    if !is_tty() {
        return Err(WizardError::NotTty);
    }

    let kind = ask_kind(partial.kind)?;
    let name = ask_name(partial.name)?;

    match kind {
        WizardKind::Linux => {
            let result = run_linux(name, partial.iso_path, partial.instances_root, advanced)?;
            Ok(WizardResult::Linux(result.0, result.1))
        }
        WizardKind::Android => {
            let result = run_android(name, partial.base_image_path, partial.instances_root, advanced)?;
            Ok(WizardResult::Android(result))
        }
    }
}

// ---------------------------------------------------------------------------
// Частично переданные через CLI аргументы
// ---------------------------------------------------------------------------

/// То, что пользователь уже мог передать через CLI-флаги до запуска wizard'а.
#[derive(Default)]
pub struct PartialArgs {
    pub kind: Option<WizardKind>,
    pub name: Option<String>,
    pub iso_path: Option<String>,
    pub base_image_path: Option<String>,
    pub instances_root: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WizardKind {
    Linux,
    Android,
}

// ---------------------------------------------------------------------------
// Linux путь
// ---------------------------------------------------------------------------

fn run_linux(
    name: String,
    iso_path: Option<String>,
    instances_root: Option<String>,
    advanced: bool,
) -> Result<(CreateInstanceRequest, String), WizardError> {
    let iso = ask_iso_path(iso_path)?;
    let disk_size_gib = ask_disk_size(256)?;
    let disk_name = format!("{name}-disk");
    let disk_path = ensure_qcow2_extension(&PathBuf::from(&disk_name))
        .to_string_lossy()
        .into_owned();

    let cdrom_bus = if iso.is_empty() {
        andler_core::CdromBus::Ide
    } else {
        let recommended =
            andler_core::CdromBus::recommended_for_iso_filename(std::path::Path::new(&iso));
        if advanced {
            ask_cdrom_bus(&iso, recommended)?
        } else {
            recommended
        }
    };

    let compact_on_shutdown = if advanced {
        ask_compact_on_shutdown()?
    } else {
        false
    };

    let root = instances_root.unwrap_or_else(|| {
        andler_core::paths::instances_root()
            .to_string_lossy()
            .into_owned()
    });

    // Авто-детект OVMF
    let ovmf_vars_template = detect_ovmf_vars_template();

    // Сводка перед созданием
    println!();
    println!("┌─ Сводка перед созданием ─────────────────────────────────────┐");
    println!("│  Тип:               Linux VM");
    println!("│  Имя:               {name}");
    if iso.is_empty() {
        println!("│  ISO:               (без ISO — загрузка с диска)");
    } else {
        println!("│  ISO:               {iso}");
        println!("│  CD-ROM bus:        {cdrom_bus:?}");
    }
    println!("│  Диск:              {disk_path}  ({disk_size_gib} GiB, qcow2)");
    println!("│  Compact on shutdown: {compact_on_shutdown}");
    println!("│  OVMF VARS:         {ovmf_vars_template}");
    println!("└───────────────────────────────────────────────────────────────┘");
    println!();

    if !confirm("Создать VM?", true)? {
        return Err(WizardError::Cancelled);
    }

    // Собираем запрос
    let mut disk = andler_core::DiskConfig::reference_default(PathBuf::from(&disk_path));
    disk.size_bytes = disk_size_gib
        .checked_mul(andler_core::DiskConfig::GIB)
        .expect("disk size overflow");
    disk.compact_on_shutdown = compact_on_shutdown;

    let mut req = CreateInstanceRequest {
        name,
        iso_path: iso,
        cpu: Some(andler_core::CpuConfig::reference_default().into()),
        memory: Some(andler_core::MemoryConfig::reference_default().into()),
        disk: Some(disk.into()),
        display: Some(andler_core::DisplayConfig::reference_default().into()),
        gpu: Some(andler_core::GpuConfig::reference_default().into()),
        network: Some(andler_core::NetworkConfig::reference_default().into()),
        firmware: Some(
            andler_core::FirmwareConfig {
                ovmf_code_path: PathBuf::new(), // daemon подставит auto-detected
                ovmf_vars_path: PathBuf::from(&ovmf_vars_template),
            }
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
        ..Default::default()
    };
    req.set_cdrom_bus(cdrom_bus.into());

    Ok((req, root))
}

// ---------------------------------------------------------------------------
// Android путь
// ---------------------------------------------------------------------------

fn run_android(
    name: String,
    base_image_path: Option<String>,
    instances_root: Option<String>,
    advanced: bool,
) -> Result<CreateAndroidInstanceRequest, WizardError> {
    let base_image = ask_base_image(base_image_path)?;
    let version = ask_android_version()?;
    let overlay_gib = ask_disk_size(20)?; // overlay поменьше — только дельта

    let gapps = if advanced {
        confirm("Установить GApps (Google Apps)?", false)?
    } else {
        false
    };
    let microg = if advanced && !gapps {
        confirm("Установить MicroG (замена GApps, без Google)?", false)?
    } else {
        false
    };
    let libndk = if advanced {
        confirm("Включить libndk (ARM-транслятор для ARMv8 приложений)?", false)?
    } else {
        true // безопасный дефолт — libndk почти всегда нужен
    };

    let (root_mode, magisk_dir) = if advanced {
        ask_root_mode()?
    } else {
        (CliRootMode::None, String::new())
    };

    let root = instances_root.unwrap_or_else(|| {
        andler_core::paths::instances_root()
            .to_string_lossy()
            .into_owned()
    });

    let ovmf_vars_template = detect_ovmf_vars_template();

    println!();
    println!("┌─ Сводка перед созданием ─────────────────────────────────────┐");
    println!("│  Тип:               Android VM");
    println!("│  Имя:               {name}");
    println!("│  Base image:        {base_image}");
    println!("│  Android:           {version:?}");
    println!("│  Overlay:           {overlay_gib} GiB");
    println!("│  GApps:             {gapps}");
    println!("│  MicroG:            {microg}");
    println!("│  libndk:            {libndk}");
    println!(
        "│  Root:              {}",
        if root_mode == CliRootMode::Magisk {
            format!("magisk ({magisk_dir})")
        } else {
            "none".to_string()
        }
    );
    println!("│  OVMF VARS:         {ovmf_vars_template}");
    println!("└───────────────────────────────────────────────────────────────┘");
    println!();

    if !confirm("Создать VM?", true)? {
        return Err(WizardError::Cancelled);
    }

    use andler_rpc::proto::AndroidProfile;

    let mut profile = AndroidProfile {
        gapps,
        microg,
        libndk,
        ..Default::default()
    };
    profile.set_android_version(version.into());
    profile.set_root(root_mode.into());

    Ok(CreateAndroidInstanceRequest {
        name,
        profile: Some(profile),
        base_image_path: base_image,
        instances_root: root,
        overlay_size_bytes: overlay_gib
            .checked_mul(andler_core::DiskConfig::GIB)
            .expect("overlay size overflow"),
        ovmf_vars_template,
        magisk_dir,
    })
}

// ---------------------------------------------------------------------------
// Вопросы (inquire-вызовы)
// ---------------------------------------------------------------------------

fn ask_kind(prefilled: Option<WizardKind>) -> Result<WizardKind, WizardError> {
    if let Some(k) = prefilled {
        return Ok(k);
    }
    let choice = Select::new(
        "Тип виртуальной машины:",
        vec!["Linux", "Android"],
    )
    .with_help_message(
        "Linux — любой дистрибутив с ISO-образом; \
         Android — образ с VirtIO-GPU и Waydroid",
    )
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(match choice {
        "Linux" => WizardKind::Linux,
        _ => WizardKind::Android,
    })
}

fn ask_name(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(n) = prefilled {
        return Ok(n);
    }
    Text::new("Имя VM:")
        .with_placeholder("my-linux-vm")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "Имя не может быть пустым".into(),
                ))
            } else if s.contains('/') || s.contains('\\') {
                Ok(inquire::validator::Validation::Invalid(
                    "Имя не должно содержать слэши".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_iso_path(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        return Ok(p);
    }
    Text::new("Путь к ISO-образу (Enter — пропустить, загрузка с диска):")
        .with_placeholder("/home/user/isos/cachyos.iso")
        .with_default("")
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_base_image(prefilled: Option<String>) -> Result<String, WizardError> {
    if let Some(p) = prefilled {
        return Ok(p);
    }
    Text::new("Путь к base-image Android:")
        .with_placeholder("/path/to/android-base.qcow2")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "Путь к base-image обязателен".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_android_version() -> Result<CliAndroidVersion, WizardError> {
    let choice = Select::new(
        "Версия Android:",
        vec!["Android 13 (рекомендуется)", "Android 11"],
    )
    .with_help_message("Android 13 лучше поддерживается с VirtIO-GPU/Venus")
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("Android 13") {
        CliAndroidVersion::V13
    } else {
        CliAndroidVersion::V11
    })
}

fn ask_root_mode() -> Result<(CliRootMode, String), WizardError> {
    let choice = Select::new(
        "Root-режим:",
        vec!["none", "magisk"],
    )
    .with_help_message(
        "none — без root; magisk — root через Magisk (потребуется путь к \
         распакованным бинарям Magisk)",
    )
    .prompt()
    .map_err(map_inquire_err)?;

    if choice == "magisk" {
        let dir = Text::new("Путь к каталогу с бинарями Magisk:")
            .with_placeholder("/home/user/magisk-release")
            .with_validator(|s: &str| {
                if s.trim().is_empty() {
                    Ok(inquire::validator::Validation::Invalid(
                        "Путь обязателен при root=magisk".into(),
                    ))
                } else {
                    Ok(inquire::validator::Validation::Valid)
                }
            })
            .prompt()
            .map_err(map_inquire_err)?;
        Ok((CliRootMode::Magisk, dir))
    } else {
        Ok((CliRootMode::None, String::new()))
    }
}

fn ask_disk_size(default_gib: u64) -> Result<u64, WizardError> {
    CustomType::<u64>::new("Размер диска (GiB):")
        .with_default(default_gib)
        .with_help_message(
            "Thin-provisioned qcow2 — номинальный предел, не занятое сразу место на хосте",
        )
        .with_error_message("Введите целое число, например 256")
        .prompt()
        .map_err(map_inquire_err)
}

fn ask_cdrom_bus(
    iso_name: &str,
    recommended: andler_core::CdromBus,
) -> Result<andler_core::CdromBus, WizardError> {
    // Строим список вариантов, ставя рекомендованный первым
    let virtio_label = "virtio-scsi  (быстрее, требует virtio-scsi в initrd)";
    let ide_label    = "ide          (медленнее, совместим с любым ISO)";

    let options = if recommended == andler_core::CdromBus::VirtioScsi {
        vec![virtio_label, ide_label]
    } else {
        vec![ide_label, virtio_label]
    };

    let rec_str = match recommended {
        andler_core::CdromBus::VirtioScsi => "virtio-scsi",
        andler_core::CdromBus::Ide => "ide",
    };

    let choice = Select::new(
        &format!("CD-ROM привод для {} (авто-рекомендация: {rec_str}):", iso_name),
        options,
    )
    .with_help_message(
        "virtio-scsi — быстрее, initrd современных дистрибутивов поддерживает; \
         ide — совместимо с Windows и любым неизвестным ISO без virtio-драйверов",
    )
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(if choice.starts_with("virtio") {
        andler_core::CdromBus::VirtioScsi
    } else {
        andler_core::CdromBus::Ide
    })
}

fn ask_compact_on_shutdown() -> Result<bool, WizardError> {
    Confirm::new("Автоматически компактировать диск после каждого выключения?")
        .with_default(false)
        .with_help_message(
            "Экономит место, но перезаписывает файл диска целиком — может занять время \
             на больших дисках. Можно включить позже через `andler config`",
        )
        .prompt()
        .map_err(map_inquire_err)
}

fn confirm(msg: &str, default: bool) -> Result<bool, WizardError> {
    Confirm::new(msg)
        .with_default(default)
        .prompt()
        .map_err(map_inquire_err)
}

// ---------------------------------------------------------------------------
// Утилиты
// ---------------------------------------------------------------------------

/// Авто-детект OVMF_VARS template для использования в запросе.
/// Возвращает пустую строку если не найдено — daemon сам подберёт.
fn detect_ovmf_vars_template() -> String {
    match andler_firmware::detect_matched_pair() {
        Ok(found) => found.vars_template.to_string_lossy().into_owned(),
        Err(_) => {
            // Не найдено — daemon при создании инстанса попробует сам
            // через свой авто-детект; здесь только warn без abort.
            eprintln!(
                "⚠  OVMF VARS не найден в стандартных путях — \
                 установите edk2-ovmf/ovmf или укажите путь явно через \
                 `andler create --ovmf-vars-template <path>`"
            );
            String::new()
        }
    }
}

/// `atty`-независимая проверка: `inquire` сам бросит `NotTTY`, но проверяем
/// заранее, чтобы вывести правильное сообщение об ошибке раньше.
fn is_tty() -> bool {
    // stdin должен быть TTY для интерактивного ввода
    use std::os::unix::io::AsRawFd;
    // SAFETY: стандартный fd, всегда валиден
    unsafe { libc_isatty(std::io::stdin().as_raw_fd()) }
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    // Минимальная проверка через syscall, без лишних зависимостей
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(fd) != 0 }
}

fn map_inquire_err(e: InquireError) -> WizardError {
    match e {
        InquireError::NotTTY => WizardError::NotTty,
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            WizardError::Cancelled
        }
        other => WizardError::Inquire(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    /// Wizard не может работать без интерактивного терминала.
    /// Пользователю нужно передать параметры через флаги или `--file`.
    #[error(
        "интерактивный wizard недоступен (нет TTY).\n\
         Передайте параметры явно:\n\
         \x20 andler create --kind linux --name <name> --iso-path <path> \
         --disk-path <path> --ovmf-vars-template <path>\n\
         Или используйте TOML-файл:\n\
         \x20 andler create --file vm.toml"
    )]
    NotTty,

    /// Пользователь отменил wizard (Esc / Ctrl-C).
    #[error("wizard отменён")]
    Cancelled,

    /// Ошибка библиотеки inquire, не отнесённая к более конкретным вариантам.
    #[error("ошибка wizard: {0}")]
    Inquire(String),
}
