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
/// Уменьшение размера (`new_size_bytes` меньше текущего) поддерживается
/// `qemu-img`, но рискованно для файловых систем гостя, которые этого не
/// ожидают — ответственность за то, что уменьшение безопасно, лежит на
/// вызывающей стороне (`andler-daemon`), не на этой функции.
pub async fn resize(path: &Path, new_size_bytes: u64) -> Result<(), DiskError> {
    run_qemu_img(&[
        "resize",
        &path.to_string_lossy(),
        &new_size_bytes.to_string(),
    ])
    .await
}

/// Сжимает диск, удаляя свободные блоки (актуально после удаления данных
/// внутри гостя — без compact файл не уменьшится физически, даже если
/// внутри гостя места было освобождено).
///
/// Реализовано как `qemu-img convert` во временный файл с последующей
/// заменой оригинала, а не `qemu-img convert -O qcow2` напрямую в тот же
/// путь — `qemu-img convert` не поддерживает source == destination.
/// Временный файл создаётся рядом с оригиналом (тот же каталог), чтобы
/// финальное переименование было атомарной операцией в пределах одной
/// файловой системы, а не межфайловым копированием.
pub async fn compact(path: &Path) -> Result<(), DiskError> {
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
/// документацию `virtual_size_bytes` выше про причину). Работает только
/// для плоских числовых полей вида `"field-name": 12345` — `qemu-img info`
/// выводит их без вложенности, так что этого достаточно на данном этапе.
fn parse_json_u64_field(json: &str, field_name: &str) -> Result<u64, DiskError> {
    let needle = format!("\"{field_name}\":");
    let idx = json.find(&needle).ok_or_else(|| {
        DiskError::ParseError(format!("field `{field_name}` not found in qemu-img output"))
    })?;

    let after = &json[idx + needle.len()..];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_u64_field_extracts_value() {
        let json = r#"{"virtual-size": 42949672960, "actual-size": 1234567}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 42949672960);
        assert_eq!(parse_json_u64_field(json, "actual-size").unwrap(), 1234567);
    }

    #[test]
    fn parse_json_u64_field_missing_field_is_parse_error() {
        let json = r#"{"virtual-size": 42949672960}"#;
        let err = parse_json_u64_field(json, "actual-size").unwrap_err();
        assert!(matches!(err, DiskError::ParseError(_)));
    }

    #[test]
    fn parse_json_u64_field_handles_field_without_following_space() {
        // qemu-img иногда выводит компактный JSON без пробела после ':'.
        let json = r#"{"virtual-size":42949672960}"#;
        assert_eq!(parse_json_u64_field(json, "virtual-size").unwrap(), 42949672960);
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
}
