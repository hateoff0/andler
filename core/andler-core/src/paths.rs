//! Единая точка резолва путей ANDLER.
//!
//! См. PLAN.md, раздел «Структура хранения»: до этого модуля каждый
//! потребитель (`cli`, `daemon`) резолвил свой кусок пути отдельно
//! (`cli/src/main.rs::default_instances_root`,
//! `daemon/src/main.rs::default_store_path`,
//! `cli/src/instance_file.rs::default_instances_root` — три независимые
//! копии одной и той же логики `dirs::data_local_dir().join("andler/...")`).
//! Это означает, что изменение дефолтного корневого пути требует правок
//! в N местах, и эти места рискуют разойтись. Здесь — один источник
//! истины: `andler_home()` и производные от него подкаталоги.
//!
//! ## Резолв `ANDLER_HOME`
//!
//! 1. Если задана переменная окружения `ANDLER_HOME` — используется она
//!    как есть (без дополнительной проверки существования: создание при
//!    необходимости — забота вызывающего кода, не этого модуля).
//! 2. Иначе — `dirs::data_local_dir()/andler`, с фоллбэком на
//!    `~/.local/share/andler`, если `data_local_dir()` не смог
//!    определиться (нет `$HOME`/`$XDG_DATA_HOME`, экзотическое
//!    окружение).
//!
//! ## Важно: миграции нет
//!
//! `ANDLER_HOME` (и любой из путей ниже) влияет только на *новые*
//! инстансы — путь конкретного инстанса фиксируется на момент его
//! создания и хранится в `InstanceConfig`. Смена `ANDLER_HOME` после
//! того, как инстансы уже созданы, не переносит и не находит файлы по
//! старому пути автоматически. Это осознанное решение, см. PLAN.md.

use std::path::PathBuf;

/// Переменная окружения, которой можно переопределить корневую
/// директорию ANDLER целиком.
pub const ANDLER_HOME_ENV: &str = "ANDLER_HOME";

/// Корневая директория ANDLER.
///
/// `$ANDLER_HOME`, если задана и непуста; иначе
/// `dirs::data_local_dir()/andler`; иначе `~/.local/share/andler`.
pub fn andler_home() -> PathBuf {
    if let Ok(value) = std::env::var(ANDLER_HOME_ENV) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("andler")
}

/// `<ANDLER_HOME>/instances` — корень для директорий отдельных
/// инстансов (`<instances_root>/<InstanceId>/disk.qcow2`, `VARS.fd`, ...).
pub fn instances_root() -> PathBuf {
    andler_home().join("instances")
}

/// `<ANDLER_HOME>/base-images` — разделяемые базовые образы (Android,
/// Linux), на которые могут ссылаться несколько инстансов.
pub fn base_images_dir() -> PathBuf {
    andler_home().join("base-images")
}

/// `<ANDLER_HOME>/ovmf` — кэш найденных/скачанных системных OVMF-файлов
/// (`OVMF_CODE.fd`, шаблон `OVMF_VARS.fd`).
pub fn ovmf_cache_dir() -> PathBuf {
    andler_home().join("ovmf")
}

/// `<ANDLER_HOME>/venus-cache` — кэш шейдеров Venus.
pub fn venus_cache_dir() -> PathBuf {
    andler_home().join("venus-cache")
}

/// `<ANDLER_HOME>/andlerd.db` — единая SQLite база состояния daemon'а
/// (не per-instance).
pub fn db_path() -> PathBuf {
    andler_home().join("andlerd.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var` мутирует процесс-глобальное состояние — тесты
    // этого модуля должны выполняться строго последовательно, иначе они
    // будут гоняться друг за другом за значением `ANDLER_HOME`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn andler_home_respects_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(ANDLER_HOME_ENV, "/tmp/andler-test-home");
        assert_eq!(andler_home(), PathBuf::from("/tmp/andler-test-home"));
        std::env::remove_var(ANDLER_HOME_ENV);
    }

    #[test]
    fn andler_home_ignores_empty_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(ANDLER_HOME_ENV, "");
        assert_ne!(andler_home(), PathBuf::from(""));
        std::env::remove_var(ANDLER_HOME_ENV);
    }

    #[test]
    fn derived_paths_are_nested_under_andler_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(ANDLER_HOME_ENV, "/tmp/andler-test-home");
        assert_eq!(
            instances_root(),
            PathBuf::from("/tmp/andler-test-home/instances")
        );
        assert_eq!(
            base_images_dir(),
            PathBuf::from("/tmp/andler-test-home/base-images")
        );
        assert_eq!(
            ovmf_cache_dir(),
            PathBuf::from("/tmp/andler-test-home/ovmf")
        );
        assert_eq!(
            venus_cache_dir(),
            PathBuf::from("/tmp/andler-test-home/venus-cache")
        );
        assert_eq!(
            db_path(),
            PathBuf::from("/tmp/andler-test-home/andlerd.db")
        );
        std::env::remove_var(ANDLER_HOME_ENV);
    }
}
