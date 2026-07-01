//! Операции с qcow2-дисками через внешний процесс `qemu-img`.
//!
//! Намеренно процесс, а не библиотека (см. README этого крейта) — это
//! означает, что каждая функция здесь асинхронно запускает `qemu-img` и
//! парсит его stdout/stderr/код возврата, а не вызывает C-API.
//!
//! Все функции принимают уже готовые пути (`&Path`) и ничего не знают про
//! `InstanceConfig`/`AndroidProfile` — это сознательная граница: домен
//! (`andler-core`) решает, какие пути и размеры нужны, а этот модуль умеет
//! только превращать конкретные параметры в реальные файлы на диске.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::error::DiskError;

/// Disk information returned by `qemu-img info --output=json`.
pub struct DiskInfo {
    /// Logical (virtual) size in bytes — what the guest OS sees.
    pub virtual_size: u64,
    /// Actual size on host in bytes — real disk usage (thin-provisioned).
    pub actual_size: u64,
    /// Disk format (e.g. "qcow2", "raw").
    pub format: String,
    /// Backing file path, if any.
    pub backing_file: Option<String>,
}

/// Создаёт новый qcow2-файл заданного логического размера (в байтах) без
/// `backing_file` — обычный самостоятельный диск.
///
/// Соответствует `qemu-img create -f qcow2 <path> <size>`.
pub async fn create(path: &Path, size_bytes: u64) -> Result<(), DiskError> {
    ensure_parent_dir_exists(path).await?;

    run_qemu_img(&[
        "create",
        "-f",
        "qcow2",
        &path.to_string_lossy(),
        &size_bytes.to_string(),
    ])
    .await
}

/// Создаёт qcow2-overlay с `backing_file`, указывающим на уже существующий
/// базовый образ. Размер overlay указывается отдельно от размера базового
/// образа — он может быть больше (типичный случай при росте раздела внутри
/// гостя поверх неизменного базового образа).
///
/// Соответствует `qemu-img create -f qcow2 -F qcow2 -b <backing_file> <path> <size>`.
/// `-F qcow2` фиксирует формат базового образа явно — без этого `qemu-img`
/// должен бы был его автоопределять, что менее предсказуемо в скрипте,
/// которым управляет другой процесс (`andlerd`), а не человек в терминале.
///
/// Проверяет существование `backing_file` заранее и возвращает
/// `DiskError::BackingFileNotFound`, если файла нет — без этой проверки
/// ошибка `qemu-img` была бы технически верной, но менее понятной при
/// диагностике (см. §4.4.2 docs/architecture/CORE_ARCHITECTURE_PLAN.md).
pub async fn create_with_backing_file(
    path: &Path,
    backing_file: &Path,
    size_bytes: u64,
) -> Result<(), DiskError> {
    if !backing_file.exists() {
        return Err(DiskError::BackingFileNotFound(backing_file.to_path_buf()));
    }

    ensure_parent_dir_exists(path).await?;

    run_qemu_img(&[
        "create",
        "-f",
        "qcow2",
        "-F",
        "qcow2",
        "-b",
        &backing_file.to_string_lossy(),
        &path.to_string_lossy(),
        &size_bytes.to_string(),
    ])
    .await
}

/// Полностью клонирует существующий диск в новый независимый файл (без
/// backing-связи с оригиналом) — в отличие от `create_with_backing_file`,
/// результат не делит данные с источником.
///
/// Реализовано как `qemu-img convert -f qcow2 -O qcow2 <source> <dest>`,
/// а не `cp` — `convert` сохраняет thin-provisioning (копирует только
/// реально записанные блоки), `cp` скопировал бы файл побайтово включая
/// неиспользуемые дыры в зависимости от поддержки sparse-файлов файловой
/// системой хоста.
pub async fn clone_full(source: &Path, dest: &Path) -> Result<(), DiskError> {
    ensure_parent_dir_exists(dest).await?;

    run_qemu_img(&[
        "convert",
        "-f",
        "qcow2",
        "-O",
        "qcow2",
        &source.to_string_lossy(),
        &dest.to_string_lossy(),
    ])
    .await
}

/// Изменяет логический размер существующего диска. `qemu-img resize`
/// принимает либо абсолютный новый размер, либо относительное изменение
/// (`+10G`) — здесь намеренно только абсолютный размер в байтах
/// (`new_size_bytes`), чтобы вызывающая сторона не передавала строки с
/// произвольным синтаксисом `qemu-img` напрямую в этот API.
///
/// Уменьшение размера (`new_size_bytes` меньше текущего виртуального
/// размера) **не выполняется без явного `allow_shrink = true`** —
/// возвращается `DiskError::ShrinkRequiresConfirmation`. Это не просто
/// проброс `--shrink` флага `qemu-img`: уменьшение рискованно для
/// файловых систем гостя, которые этого не ожидают (см. PLAN.md, раздел
/// «Disk management»), и должно требовать осознанного подтверждения
/// вызывающей стороны (CLI/wizard), а не приниматься как любое другое
/// изменение размера. При `allow_shrink = true` к команде добавляется
/// `--shrink`, которого иначе `qemu-img` потребовал бы сам с менее
/// понятным сообщением об ошибке.
pub async fn resize(path: &Path, new_size_bytes: u64, allow_shrink: bool) -> Result<(), DiskError> {
    let current_size_bytes = virtual_size_bytes(path).await?;

    if new_size_bytes < current_size_bytes && !allow_shrink {
        return Err(DiskError::ShrinkRequiresConfirmation {
            path: path.to_path_buf(),
            current_size_bytes,
            requested_size_bytes: new_size_bytes,
        });
    }

    let size_arg = new_size_bytes.to_string();
    let path_arg = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["resize"];
    if new_size_bytes < current_size_bytes {
        args.push("--shrink");
    }
    args.push(&path_arg);
    args.push(&size_arg);

    run_qemu_img(&args).await
}

/// Сжимает диск, удаляя свободные блоки (актуально после удаления данных
/// внутри гостя — без compact файл не уменьшится физически, даже если
/// внутри гостя места было освобождено).
///
/// Применимо только к **qcow2**: raw не имеет qcow2-метаданных, которые
/// можно было бы компактифицировать (см. PLAN.md, раздел «Disk
/// management»). Для любого другого формата возвращает
/// `DiskError::CompactNotApplicable` — не пытается слепо выполнить
/// `qemu-img convert` и не оставляет вызывающей стороне разбираться с
/// менее понятной ошибкой `qemu-img` (или, хуже, молча создавать
/// бессмысленный для raw файл).
///
/// Реализовано как `qemu-img convert` во временный файл с последующей
/// заменой оригинала, а не `qemu-img convert -O qcow2` напрямую в тот же
/// путь — `qemu-img convert` не поддерживает source == destination.
/// Временный файл создаётся рядом с оригиналом (тот же каталог), чтобы
/// финальное переименование было атомарной операцией в пределах одной
/// файловой системы, а не межфайловым копированием.
pub async fn compact(path: &Path) -> Result<(), DiskError> {
    let disk_info = info(path).await?;
    if disk_info.format != "qcow2" {
        return Err(DiskError::CompactNotApplicable {
            path: path.to_path_buf(),
            format: disk_info.format,
        });
    }

    let tmp_path = path.with_extension("qcow2.compact-tmp");

    run_qemu_img(&[
        "convert",
        "-f",
        "qcow2",
        "-O",
        "qcow2",
        &path.to_string_lossy(),
        &tmp_path.to_string_lossy(),
    ])
    .await?;

    tokio::fs::rename(&tmp_path, path)
        .await
        .map_err(|source| DiskError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Логический (виртуальный) размер диска в байтах — то, что видит гостевая
/// ОС, не фактическое занятое место на хосте (см. `disk_usage_bytes` для
/// последнего).
///
/// Получено через `qemu-img info --output=json` и парсинг поля
/// `virtual-size` — без подключения полноценного JSON-парсера: на этом
/// этапе используется простой текстовый поиск поля, так как `andler-disk`
/// не имеет иных причин тащить `serde_json` (он уже есть только как
/// dev-dependency в `andler-core`, не как продакшен-зависимость здесь).
/// Если набор полей, которые нужно читать из `qemu-img info`, вырастет —
/// стоит пересмотреть это решение в пользу настоящего JSON-парсинга.
pub async fn virtual_size_bytes(path: &Path) -> Result<u64, DiskError> {
    let output = run_qemu_img_capturing_stdout(&[
        "info",
        "--output=json",
        &path.to_string_lossy(),
    ])
    .await?;

    parse_json_u64_field(&output, "virtual-size")
}

/// Фактически занятое место на хосте в байтах (поле `actual-size` в выводе
/// `qemu-img info`) — отличается от `virtual_size_bytes` для
/// thin-provisioned дисков, где занятое место растёт по мере записи данных
/// внутри гостя, а не сразу до полного логического размера.
pub async fn disk_usage_bytes(path: &Path) -> Result<u64, DiskError> {
    let output = run_qemu_img_capturing_stdout(&[
        "info",
        "--output=json",
        &path.to_string_lossy(),
    ])
    .await?;

    parse_json_u64_field(&output, "actual-size")
}

/// Полная информация о диске — один вызов `qemu-img info --output=json`.
pub async fn info(path: &Path) -> Result<DiskInfo, DiskError> {
    let output = run_qemu_img_capturing_stdout(&[
        "info",
        "--output=json",
        &path.to_string_lossy(),
    ])
    .await?;

    let virtual_size = parse_json_u64_field(&output, "virtual-size")?;
    let actual_size = parse_json_u64_field(&output, "actual-size")?;
    let format = parse_json_string_field(&output, "format")?;
    let backing_file = parse_json_optional_string_field(&output, "backing-filename");

    Ok(DiskInfo {
        virtual_size,
        actual_size,
        format,
        backing_file,
    })
}

/// Запускает `qemu-img` с заданными аргументами, не возвращая stdout —
/// для команд, где важен только успех/неуспех (`create`, `resize`,
/// `convert`).
async fn run_qemu_img(args: &[&str]) -> Result<(), DiskError> {
    run_qemu_img_capturing_stdout(args).await.map(|_| ())
}

/// Запускает `qemu-img` с заданными аргументами и возвращает stdout как
/// строку — для команд, чей результат нужен вызывающей стороне (`info`).
async fn run_qemu_img_capturing_stdout(args: &[&str]) -> Result<String, DiskError> {
    let output = Command::new("qemu-img")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(DiskError::SpawnFailed)?;

    if !output.status.success() {
        return Err(DiskError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Создаёт родительский каталог пути, если он ещё не существует —
/// `qemu-img create`/`convert` не делают этого сами и завершаются с
/// ошибкой, если каталог отсутствует.
async fn ensure_parent_dir_exists(path: &Path) -> Result<(), DiskError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| DiskError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
    }
    Ok(())
}

/// Минималистичный извлекатель числового поля из JSON-вывода `qemu-img
/// info --output=json` без подключения полноценного парсера (см.
/// документацию `virtual_size_bytes` выше про причину). Поддерживает
/// оба формата: `"field": 12345` (с пробелом) и `"field":12345` (без).
fn parse_json_u64_field(json: &str, field_name: &str) -> Result<u64, DiskError> {
    let needle_with_space = format!("\"{field_name}\": ");
    let needle_no_space = format!("\"{field_name}\":");

    let needle_len;
    let idx = if let Some(i) = json.find(&needle_with_space) {
        needle_len = needle_with_space.len();
        i
    } else if let Some(i) = json.find(&needle_no_space) {
        needle_len = needle_no_space.len();
        i
    } else {
        return Err(DiskError::ParseError(format!(
            "field `{field_name}` not found in qemu-img output"
        )));
    };

    let after = &json[idx + needle_len..];
    let value_str: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();

    value_str.parse::<u64>().map_err(|_| {
        DiskError::ParseError(format!(
            "field `{field_name}` is not a valid u64 in qemu-img output"
        ))
    })
}

/// Извлекает строковое поле из JSON (например `"format": "qcow2"`).
/// Поддерживает оба формата: `"field": "value"` и `"field":"value"`.
fn parse_json_string_field(json: &str, field_name: &str) -> Result<String, DiskError> {
    let needle_with_space = format!("\"{field_name}\": \"");
    let needle_no_space = format!("\"{field_name}\":\"");

    let needle_len;
    let idx = if let Some(i) = json.find(&needle_with_space) {
        needle_len = needle_with_space.len();
        i
    } else if let Some(i) = json.find(&needle_no_space) {
        needle_len = needle_no_space.len();
        i
    } else {
        return Err(DiskError::ParseError(format!(
            "field `{field_name}` not found in qemu-img output"
        )));
    };

    let after = &json[idx + needle_len..];
    let value: String = after.chars().take_while(|&c| c != '"').collect();

    if value.is_empty() {
        return Err(DiskError::ParseError(format!(
            "field `{field_name}` is empty in qemu-img output"
        )));
    }

    Ok(value)
}

/// Извлекает опциональное строковое поле из JSON (возвращает `None` если
/// отсутствует).
fn parse_json_optional_string_field(json: &str, field_name: &str) -> Option<String> {
    parse_json_string_field(json, field_name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_json_u64_field tests --

    #[test]
    fn parse_json_u64_field_extracts_value_compact() {
        let json = r#"{"virtual-size":42949672960,"actual-size":1234567}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 42949672960);
        assert_eq!(parse_json_u64_field(json, "actual-size").unwrap(), 1234567);
    }

    #[test]
    fn parse_json_u64_field_extracts_value_spaced() {
        let json = r#"{"virtual-size": 42949672960, "actual-size": 1234567}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 42949672960);
        assert_eq!(parse_json_u64_field(json, "actual-size").unwrap(), 1234567);
    }

    #[test]
    fn parse_json_u64_field_missing_field_is_parse_error() {
        let json = r#"{"virtual-size":42949672960}"#;
        let err = parse_json_u64_field(json, "actual-size").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_u64_field_zero_value() {
        let json = r#"{"virtual-size":0}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 0);
    }

    #[test]
    fn parse_json_u64_field_nested_json_matches_top_level() {
        let json = r#"{"children":[{"info":{"virtual-size":197120,"format":"file"}}],"virtual-size":1048576,"format":"qcow2"}"#;
        // find() returns the first match — which is the nested one.
        // This documents the known limitation of flat find().
        let result = parse_json_u64_field(json, "virtual-size").unwrap();
        assert_eq!(result, 197120, "flat find() matches nested field first — known limitation");
    }

    // -- parse_json_string_field tests --

    #[test]
    fn parse_json_string_field_extracts_value_compact() {
        let json = r#"{"format":"qcow2","backing-filename":"/path/to/base.qcow2"}"#;
        assert_eq!(parse_json_string_field(json, "format").unwrap(), "qcow2");
        assert_eq!(
            parse_json_string_field(json, "backing-filename").unwrap(),
            "/path/to/base.qcow2"
        );
    }

    #[test]
    fn parse_json_string_field_extracts_value_spaced() {
        let json = r#"{"format": "qcow2", "backing-filename": "/path/to/base.qcow2"}"#;
        assert_eq!(parse_json_string_field(json, "format").unwrap(), "qcow2");
        assert_eq!(
            parse_json_string_field(json, "backing-filename").unwrap(),
            "/path/to/base.qcow2"
        );
    }

    #[test]
    fn parse_json_string_field_missing_field_is_error() {
        let json = r#"{"format":"qcow2"}"#;
        let err = parse_json_string_field(json, "backing-filename").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_string_field_empty_value_is_error() {
        let json = r#"{"format":""}"#;
        let err = parse_json_string_field(json, "format").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_string_field_empty_value_with_space_is_error() {
        let json = r#"{"format": ""}"#;
        let err = parse_json_string_field(json, "format").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    // -- parse_json_optional_string_field tests --

    #[test]
    fn parse_json_optional_string_field_returns_value_compact() {
        let json = r#"{"backing-filename":"/path/to/base.qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            Some("/path/to/base.qcow2".to_string())
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_value_spaced() {
        let json = r#"{"backing-filename": "/path/to/base.qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            Some("/path/to/base.qcow2".to_string())
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_none_when_missing() {
        let json = r#"{"format":"qcow2"}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            None
        );
    }

    #[test]
    fn parse_json_optional_string_field_returns_none_when_empty() {
        let json = r#"{"backing-filename":""}"#;
        assert_eq!(
            parse_json_optional_string_field(json, "backing-filename"),
            None
        );
    }

    // Тесты, которым реально нужен бинарник `qemu-img` (create/clone/resize/
    // compact/info на настоящих файлах), требуют `qemu-utils` в окружении —
    // помечены #[ignore] и гоняются в integration-test Docker-таргете
    // (см. docker/README.md), а не в unit-test.

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_then_virtual_size_round_trips() {
        let dir = std::env::temp_dir().join("andler-disk-test-create");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 10 * 1024 * 1024 * 1024).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 10 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn create_with_backing_file_fails_fast_on_missing_backing() {
        let dir = std::env::temp_dir().join("andler-disk-test-missing-backing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let overlay_path = dir.join("overlay.qcow2");
        let missing_backing = dir.join("does-not-exist.qcow2");

        let err = create_with_backing_file(&overlay_path, &missing_backing, 10 * 1024 * 1024 * 1024)
            .await
            .unwrap_err();
        assert!(matches!(err, DiskError::BackingFileNotFound(_)));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_grow_succeeds_without_confirmation() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-grow");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 10 * 1024 * 1024 * 1024).await.unwrap();
        resize(&path, 20 * 1024 * 1024 * 1024, false).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 20 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_shrink_without_confirmation_is_rejected() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-shrink-reject");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 20 * 1024 * 1024 * 1024).await.unwrap();
        let err = resize(&path, 10 * 1024 * 1024 * 1024, false)
            .await
            .unwrap_err();
        assert!(matches!(err, DiskError::ShrinkRequiresConfirmation { .. }));
        // Размер не должен был измениться — отказ происходит до вызова
        // `qemu-img resize`.
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 20 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn resize_shrink_with_confirmation_succeeds() {
        let dir = std::env::temp_dir().join("andler-disk-test-resize-shrink-confirmed");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 20 * 1024 * 1024 * 1024).await.unwrap();
        resize(&path, 10 * 1024 * 1024 * 1024, true).await.unwrap();
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 10 * 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn compact_qcow2_succeeds() {
        let dir = std::env::temp_dir().join("andler-disk-test-compact-qcow2");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.qcow2");

        create(&path, 1024 * 1024 * 1024).await.unwrap();
        compact(&path).await.unwrap();
        // Файл остаётся валидным qcow2 того же логического размера.
        let size = virtual_size_bytes(&path).await.unwrap();
        assert_eq!(size, 1024 * 1024 * 1024);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    #[ignore = "requires qemu-img binary, see docker/README.md integration-test target"]
    async fn compact_raw_is_rejected_as_not_applicable() {
        let dir = std::env::temp_dir().join("andler-disk-test-compact-raw");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("test.raw");

        run_qemu_img(&[
            "create",
            "-f",
            "raw",
            &path.to_string_lossy(),
            &(1024 * 1024 * 1024).to_string(),
        ])
        .await
        .unwrap();

        let err = compact(&path).await.unwrap_err();
        assert!(matches!(
            err,
            DiskError::CompactNotApplicable { format, .. } if format == "raw"
        ));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
