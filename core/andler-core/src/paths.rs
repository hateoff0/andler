use std::path::PathBuf;

pub const ANDLER_HOME_ENV: &str = "ANDLER_HOME";

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

pub fn instances_root() -> PathBuf {
    andler_home().join("instances")
}

pub fn base_images_dir() -> PathBuf {
    andler_home().join("cache/base-images")
}

pub fn arm_translators_dir() -> PathBuf {
    andler_home().join("cache/arm-translators")
}

pub fn db_path() -> PathBuf {
    andler_home().join("andlerd.db")
}

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

pub fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid(2) is a pure POSIX call — returns the real user ID, no arguments.
    unsafe { getuid() }
}

pub async fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    ensure_private_dir_sync(dir)
}

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
        assert_eq!(db_path(), PathBuf::from("/tmp/andler-test-home/andlerd.db"));
        std::env::remove_var(ANDLER_HOME_ENV);
    }

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
