//! Запуск и базовое управление процессом `qemu-system-x86_64`.
//!
//! Этот модуль умеет только то, что не требует QMP: запустить процесс с
//! заданными аргументами, проверить, жив ли он, и принудительно завершить
//! (`SIGKILL`-подобное поведение `Child::kill`). Graceful shutdown через
//! ACPI/QMP `system_powerdown` — задача `qmp.rs` (следующий шаг реализации,
//! см. README этого крейта); пока его нет, `stop(graceful=true)` на уровне
//! `backend.rs` этого крейта реализован через `SIGTERM` + ожидание с
//! таймаутом, см. документацию `terminate` ниже.
//!
//! `process.rs` не знает про `InstanceConfig`/`BackendHandle` — это работа
//! `backend.rs`, который связывает результат `cmdline::build_args` и этот
//! модуль с интерфейсом `HypervisorBackend`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Имя бинарника QEMU. Константа, а не конфигурация — на этом этапе нет
/// причины делать путь к `qemu-system-x86_64` настраиваемым; если он не
/// найден в `$PATH`, `spawn` вернёт `ProcessError::SpawnFailed`.
const QEMU_BINARY: &str = "qemu-system-x86_64";

/// Сколько ждать после `SIGTERM`, прежде чем считать graceful-остановку
/// неудавшейся (см. документацию `terminate`). Не настраивается через
/// `InstanceConfig` — это деталь способа остановки процесса, не часть
/// декларативной конфигурации инстанса.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ProcessError {
    /// Не удалось запустить процесс (бинарник не найден, нет прав и т.п.).
    #[error("failed to spawn {QEMU_BINARY}: {0}")]
    SpawnFailed(std::io::Error),

    /// Ошибка при попытке проверить статус/завершить процесс (например,
    /// `wait()` вернул I/O-ошибку — не путать со штатным завершением с
    /// ненулевым кодом, это не ошибка с точки зрения `process.rs`).
    #[error("process I/O error: {0}")]
    Io(std::io::Error),

    /// `SIGTERM` отправлен, но процесс не завершился за
    /// `GRACEFUL_SHUTDOWN_TIMEOUT` — вызывающая сторона должна решить,
    /// эскалировать ли до принудительного `kill()` (см. документацию
    /// `terminate`).
    #[error("process did not exit within {0:?} after SIGTERM")]
    GracefulShutdownTimedOut(Duration),
}

/// Живой процесс QEMU, запущенный этим модулем.
///
/// Хранит `tokio::process::Child` — реальный хэндл ОС, который не может
/// быть сериализован и должен жить в памяти `andler-daemon`, а не
/// персистентно в `andler-store` (там хранится только `BackendHandle` —
/// см. документацию `backend.rs`, следующий модуль этого крейта).
pub struct QemuProcess {
    child: Child,
    /// PID на момент запуска — сохраняется отдельно от `child.id()`,
    /// так как `Child::id()` возвращает `None` после того, как процесс
    /// был дождан (`wait`), а PID нужен для `BackendHandle`/диагностики
    /// даже после этого.
    pid: u32,
    qmp_socket_path: PathBuf,
}

impl QemuProcess {
    /// Запускает `qemu-system-x86_64` с заданными аргументами (обычно —
    /// результат `cmdline::build_args`).
    ///
    /// stdout/stderr процесса перенаправляются в `Stdio::piped()`, а не
    /// наследуются от `andlerd` — иначе вывод множества QEMU-инстансов
    /// смешивался бы в одном терминале демона. На этом этапе вывод не
    /// читается активно (`Child` хранит дескрипторы, но никто их не
    /// drain'ит) — если QEMU выведет достаточно много в stderr и буфер
    /// ОС заполнится, процесс может заблокироваться на записи. Это
    /// известное ограничение текущего шага, не описанное как решённое:
    /// полноценное логирование вывода QEMU — отдельная задача (см. TODO
    /// в README этого крейта).
    pub async fn spawn(args: &[String], qmp_socket_path: PathBuf) -> Result<Self, ProcessError> {
        let child = Command::new(QEMU_BINARY)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(ProcessError::SpawnFailed)?;

        // `id()` возвращает `None` только если процесс уже был дождан —
        // сразу после успешного `spawn()` это не так, `expect` здесь
        // отражает инвариант библиотеки tokio, а не предположение об
        // окружении.
        let pid = child
            .id()
            .expect("freshly spawned child must have a pid");

        Ok(QemuProcess {
            child,
            pid,
            qmp_socket_path,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn qmp_socket_path(&self) -> &PathBuf {
        &self.qmp_socket_path
    }

    /// `true`, если процесс всё ещё выполняется. Не блокирует и не ждёт —
    /// `try_wait()` сразу возвращает текущий статус.
    pub async fn is_alive(&mut self) -> Result<bool, ProcessError> {
        match self.child.try_wait().map_err(ProcessError::Io)? {
            Some(_exit_status) => Ok(false),
            None => Ok(true),
        }
    }

    /// Graceful-остановка: отправляет `SIGTERM` и ждёт завершения процесса
    /// до `GRACEFUL_SHUTDOWN_TIMEOUT`.
    ///
    /// Это сознательно временная мера до появления `qmp.rs`: правильный
    /// graceful shutdown для QEMU — ACPI-сигнал через QMP
    /// (`system_powerdown`), который даёт гостевой ОС шанс корректно
    /// завершить процессы и размонтировать файловые системы. `SIGTERM`
    /// самому процессу QEMU обычно тоже приводит к чистому завершению
    /// QEMU-процесса (это не `SIGKILL`), но не эквивалентен ACPI-сигналу
    /// гостю — гостевая ОС не получает шанс отреагировать. См. README
    /// этого крейта про статус `qmp.rs`.
    ///
    /// На платформах без `nix`/Unix-сигналов (этот крейт ориентирован на
    /// Linux, см. продуктовую концепцию — ANDLER не предполагает Windows-
    /// хост) `tokio::process::Child` не даёт прямого API для `SIGTERM`
    /// без дополнительной зависимости; здесь это решено через `libc::kill`
    /// напрямую по сохранённому `pid`, без отдельного крейта-обёртки.
    pub async fn terminate(&mut self) -> Result<(), ProcessError> {
        // SAFETY: `kill(2)` с валидным pid и сигналом SIGTERM не нарушает
        // память процесса — это системный вызов, а не доступ к чужой
        // памяти. pid принадлежит процессу, который мы сами запустили в
        // `spawn()` и который ещё не был дождан (иначе `is_alive` вызвал
        // бы это раньше и вызывающая сторона не дошла бы до `terminate`).
        let kill_result = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) };
        if kill_result != 0 {
            return Err(ProcessError::Io(std::io::Error::last_os_error()));
        }

        match timeout(GRACEFUL_SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_exit_status)) => Ok(()),
            Ok(Err(io_err)) => Err(ProcessError::Io(io_err)),
            Err(_elapsed) => Err(ProcessError::GracefulShutdownTimedOut(
                GRACEFUL_SHUTDOWN_TIMEOUT,
            )),
        }
    }

    /// Принудительная остановка (`SIGKILL`-подобная — `Child::kill()`).
    /// Используется при `stop(graceful=false)` или как эскалация после
    /// неудавшегося `terminate()`.
    pub async fn force_kill(&mut self) -> Result<(), ProcessError> {
        self.child.kill().await.map_err(ProcessError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Запуск настоящего `qemu-system-x86_64` требует бинарника в
    /// окружении — недоступно в `unit-test` Docker-таргете (см.
    /// docker/README.md), помечено `#[ignore]`. `/dev/kvm` для самого
    /// `spawn`/`terminate`/`is_alive` не обязателен (можно запустить QEMU
    /// и без kvm-акселерации, просто медленнее) — здесь используется
    /// минимальный набор аргументов без `-accel kvm`, чтобы тест был
    /// пригоден и без `--device=/dev/kvm`, если бинарник присутствует.
    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn spawn_then_is_alive_then_terminate() {
        let qmp_path = std::env::temp_dir().join("andler-process-test-qmp.sock");
        let args = vec![
            "-display".to_string(),
            "none".to_string(),
            "-nographic".to_string(),
        ];

        let mut process = QemuProcess::spawn(&args, qmp_path).await.unwrap();
        assert!(process.is_alive().await.unwrap());

        process.terminate().await.unwrap();
        assert!(!process.is_alive().await.unwrap());
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn force_kill_stops_unresponsive_process() {
        let qmp_path = std::env::temp_dir().join("andler-process-test-qmp-killed.sock");
        let args = vec![
            "-display".to_string(),
            "none".to_string(),
            "-nographic".to_string(),
        ];

        let mut process = QemuProcess::spawn(&args, qmp_path).await.unwrap();
        process.force_kill().await.unwrap();
        assert!(!process.is_alive().await.unwrap());
    }

    #[tokio::test]
    async fn spawn_with_missing_binary_fails_with_spawn_error() {
        // Проверяет ветку SpawnFailed напрямую через tokio::process::Command
        // с гарантированно несуществующим бинарником — не трогает глобальный
        // PATH процесса (изменение PATH было бы небезопасно: тесты в одном
        // бинарнике выполняются параллельно по умолчанию, и это повлияло бы
        // на другие тесты, выполняющиеся в это же время). Не требует
        // qemu-system-x86_64, безопасно гонять в unit-test.
        let result = Command::new("/nonexistent-binary-for-andler-qemu-test")
            .spawn()
            .map_err(ProcessError::SpawnFailed);

        assert!(matches!(result, Err(ProcessError::SpawnFailed(_))));
    }
}
