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
//! 2. Иначе — `dirs::home_dir()/.andler`, с фоллбэком на
//!    `~/.andler`, если `home_dir()` не смог определиться
//!    (нет `$HOME`, экзотическое окружение).
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

/// `$ANDLER_HOME`, если задана и непуста; иначе
/// `dirs::home_dir()/.andler`; иначе `~/.andler`.
pub fn andler_home() -> PathBuf {
    if let Ok(value) = std::env::var(ANDLER_HOME_ENV) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".andler")
}

/// `<ANDLER_HOME>/instances` — корень для директорий отдельных
/// инстансов (`<instances_root>/<InstanceId>/disk.qcow2`, `VARS.fd`, ...).
pub fn instances_root() -> PathBuf {
    andler_home().join("instances")
}

/// `<ANDLER_HOME>/cache/base-images` — базовые образы гостей
/// (Linux/Android), на основе которых создаются overlay-диски
/// инстансов. Плейсхолдер: структура фиксируется сейчас, логика
/// скачивания — отдельная задача.
pub fn base_images_dir() -> PathBuf {
    andler_home().join("cache/base-images")
}

/// `<ANDLER_HOME>/cache/arm-translators` — кэш трансляторов
/// архитектур (libhoudini, libndk и т.п.). Плейсхолдер: структура
/// фиксируется сейчас, логика скачивания — отдельная задача.
pub fn arm_translators_dir() -> PathBuf {
    andler_home().join("cache/arm-translators")
}

/// `<ANDLER_HOME>/andlerd.db` — единая SQLite база состояния daemon'а
/// (не per-instance).
pub fn db_path() -> PathBuf {
    andler_home().join("andlerd.db")
}

/// Базовая директория для короткоживущих IPC-ресурсов текущего
/// пользователя — QMP-сокеты (`andler-qemu`), временные точки
/// монтирования и патч-каталоги Magisk-провижининга (`andler-disk`). См.
/// PLAN.md, item 20a, "Hardcoded `/tmp` paths for IPC sockets".
///
/// `$XDG_RUNTIME_DIR`, если задана (systemd user session — стандартный
/// case на большинстве современных Linux-дистрибутивов, tmpfs,
/// `0700`, привязана к сессии пользователя, автоматически очищается при
/// логауте); иначе `/run/user/<uid>`, тот же путь, который
/// `XDG_RUNTIME_DIR` обычно и указывает, на случай, если переменная не
/// экспортирована, но каталог всё равно существует (например, процесс
/// запущен не из-под полноценной сессии login); иначе (ни того, ни
/// другого) — `std::env::temp_dir()` (`/tmp` на большинстве систем) как
/// последний фоллбэк, чтобы andler вообще мог работать в
/// нестандартных/контейнерных окружениях без полноценного systemd —
/// сам вызывающий код (`andler-qemu`, `andler-disk`) обязан в этом
/// случае явно выставить права `0700` на создаваемые здесь директории
/// (см. `ensure_private_dir`), раз общий `/tmp` может быть
/// world-writable.
///
/// Раньше QMP-сокеты и Magisk-временные файлы шли прямо в
/// `/tmp/andler/...`/`/tmp/andler-mount-*` без разбора — на
/// многопользовательской системе с world-writable `/tmp` это открывает
/// symlink-атаку (см. PLAN.md за полным описанием). Использование
/// `runtime_dir()` не устраняет риск полностью на системах без
/// `XDG_RUNTIME_DIR`/`/run/user/<uid>` вообще, но устраняет его на
/// подавляющем большинстве современных Linux-хостов, где обе точки
/// присутствуют.
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    let uid_dir = PathBuf::from(format!("/run/user/{}", current_uid()));
    if uid_dir.is_dir() {
        return uid_dir;
    }

    std::env::temp_dir()
}

/// Raw `getuid(2)` FFI call — avoids pulling in the `libc` crate for a
/// single syscall. Shared here (rather than duplicated) because
/// `services/andler-firmware/src/detect/audio.rs` already needed the
/// exact same call for its own session-socket check before this
/// function existed — moved here and re-exported so there's one copy,
/// not two independently-maintained ones (see this module's own stated
/// reason for existing, in the top-of-file doc comment, about avoiding
/// exactly this kind of duplication).
pub fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Создаёт `dir` (со всеми родителями, как `create_dir_all`) и сразу
/// выставляет права `0700` — см. PLAN.md, item 20b, "No file permission
/// controls". Отдельно от `runtime_dir()` (которая только резолвит
/// путь, ничего не создаёт) — большинство мест, где нужен приватный
/// каталог, и так уже вызывают `create_dir_all` сами; эта функция
/// заменяет именно этот вызов, не добавляется поверх него.
///
/// На платформах без Unix-прав (сборка под non-Unix — маловероятно
/// для ANDLER, но `#[cfg(unix)]` явно ограничивает область) `chmod`
/// просто не делается: `PermissionsExt` недоступен вне Unix, а на
/// таких платформах и сама модель прав другая.
pub async fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    ensure_private_dir_sync(dir)
}

/// Синхронный вариант [`ensure_private_dir`] — для модулей, работающих
/// через `std::fs` напрямую (обычно потому, что сами вызываются внутри
/// `tokio::task::spawn_blocking`, где async I/O не даёт преимуществ и
/// просто добавляет накладные расходы), например
/// `services/andler-disk/src/magisk.rs`. Та же логика, тот же `0700`,
/// просто без `.await`.
pub fn ensure_private_dir_sync(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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
            PathBuf::from("/tmp/andler-test-home/cache/base-images")
        );
        assert_eq!(
            arm_translators_dir(),
            PathBuf::from("/tmp/andler-test-home/cache/arm-translators")
        );
        assert_eq!(
            db_path(),
            PathBuf::from("/tmp/andler-test-home/andlerd.db")
        );
        std::env::remove_var(ANDLER_HOME_ENV);
    }

    // `runtime_dir()`/`XDG_RUNTIME_DIR` mutate the same kind of
    // process-global env state as `ANDLER_HOME` above — reuse the same
    // lock so these tests don't race each other either (a single
    // process-wide `Mutex` covering *all* env-mutating tests in this
    // module, not a second, separate lock just for this variable).
    #[test]
    fn runtime_dir_respects_xdg_runtime_dir_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        assert_eq!(runtime_dir(), PathBuf::from("/run/user/1000"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn runtime_dir_ignores_empty_xdg_runtime_dir_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", "");
        // Falls through to /run/user/<uid> or std::env::temp_dir() —
        // either way, must not be an empty path.
        assert_ne!(runtime_dir(), PathBuf::from(""));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn ensure_private_dir_sync_creates_dir_with_0700() {
        let dir = std::env::temp_dir().join(format!(
            "andler-core-paths-test-{}-{}",
            std::process::id(),
            current_uid()
        ));
        let _ = std::fs::remove_dir_all(&dir); // clean slate if a previous run left it behind
        ensure_private_dir_sync(&dir).expect("ensure_private_dir_sync should succeed");
        assert!(dir.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            // Mask off the file-type bits, keep only the permission bits.
            assert_eq!(mode & 0o777, 0o700);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ensure_private_dir_async_creates_dir_with_0700() {
        let dir = std::env::temp_dir().join(format!(
            "andler-core-paths-test-async-{}-{}",
            std::process::id(),
            current_uid()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir)
            .await
            .expect("ensure_private_dir should succeed");
        assert!(dir.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
