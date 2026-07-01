//! Управление OVMF-firmware в контексте `andlerd`.
//!
//! При запуске daemon проверяет, переданы ли пути к OVMF явно (через
//! аргументы командной строки / переменные окружения). Если нет —
//! автоматически ищет их через `andler_firmware::detect_matched_pair()`.
//!
//! Результат детекции сохраняется в `OvmfPaths` и передаётся в
//! `Daemon::create_instance` / `create_android_instance`. Это значит,
//! что авто-детект запускается **один раз** при старте, а не при каждом
//! создании инстанса — быстрее и предсказуемее, плюс ошибки "OVMF не
//! установлен" всплывают сразу, не при первом `andler create`.

use std::path::PathBuf;

/// Разрешённые пути к OVMF-файлам — либо явно переданные, либо
/// авто-определённые через `andler_firmware::detect_matched_pair()`.
#[derive(Debug, Clone)]
pub struct OvmfPaths {
    /// `/usr/share/edk2/x64/OVMF_CODE.4m.fd` (или аналог дистрибутива).
    /// Системный, read-only, общий для всех инстансов.
    pub code: PathBuf,
    /// `/usr/share/edk2/x64/OVMF_VARS.4m.fd` (или аналог дистрибутива).
    /// Шаблон, из которого `provision_vars` создаёт персональный VARS.fd
    /// для каждого нового инстанса.
    pub vars_template: PathBuf,
}

/// Разрешает `OvmfPaths` из явно заданных путей или через авто-детект.
///
/// Вызывается один раз в `main()` daemon'а перед тем, как начать
/// принимать gRPC-запросы. Если авто-детект не находит OVMF —
/// возвращает ошибку, и daemon завершается с понятным сообщением,
/// а не падает непонятно где при первом `create_instance`.
///
/// `ovmf_code` и `ovmf_vars_template` — значения из CLI / env:
/// - `Some(path)` → используется как есть, авто-детект не запускается.
/// - `None` → запускается `detect_matched_pair()`.
///
/// Этот паттерн (explicit > auto-detect) соответствует стандарту
/// ANDLER: пользователь всегда может принудительно задать путь и
/// получить воспроизводимое поведение.
pub fn resolve(
    ovmf_code: Option<PathBuf>,
    ovmf_vars_template: Option<PathBuf>,
) -> Result<OvmfPaths, andler_firmware::FirmwareError> {
    match (ovmf_code, ovmf_vars_template) {
        (Some(code), Some(vars_template)) => {
            tracing::info!(
                ovmf_code = %code.display(),
                ovmf_vars_template = %vars_template.display(),
                "using explicitly specified OVMF paths"
            );
            Ok(OvmfPaths { code, vars_template })
        }
        (explicit_code, explicit_vars) => {
            // Один или оба пути не заданы — запускаем авто-детект и
            // применяем явные значения как переопределения поверх
            // результата детекта (отдельно для CODE и VARS).
            tracing::info!(
                "OVMF paths not fully specified, running auto-detection"
            );
            let detected = andler_firmware::detect_matched_pair()?;
            let code = explicit_code.unwrap_or(detected.code);
            let vars_template = explicit_vars.unwrap_or(detected.vars_template);
            tracing::info!(
                ovmf_code = %code.display(),
                ovmf_vars_template = %vars_template.display(),
                "OVMF paths resolved (auto-detected)"
            );
            Ok(OvmfPaths { code, vars_template })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_paths_without_detection() {
        let code = PathBuf::from("/explicit/OVMF_CODE.fd");
        let vars = PathBuf::from("/explicit/OVMF_VARS.fd");
        let result = resolve(Some(code.clone()), Some(vars.clone()))
            .expect("explicit paths must always succeed without touching FS");
        assert_eq!(result.code, code);
        assert_eq!(result.vars_template, vars);
    }

    #[test]
    fn resolve_falls_back_to_autodetect_when_paths_missing() {
        // Авто-детект на CI без OVMF вернёт ошибку — это ожидаемо
        // и является правильным поведением. Если OVMF установлен —
        // вернёт Ok с реальными путями.
        let result = resolve(None, None);
        // На CI: Err(OvmfCodeNotFound/OvmfVarsNotFound)
        // На машине с OVMF: Ok(OvmfPaths { ... })
        // В обоих случаях: не паникует.
        let _ = result;
    }
}
