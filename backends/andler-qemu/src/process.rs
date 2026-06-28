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

use andler_core::{LogLine, LogStreamSource, ResourceMetrics};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
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

/// Емкость `broadcast`-канала строк лога на один процесс (см.
/// `QemuProcess::log_sender`/`subscribe_logs`).
///
/// `broadcast::channel` не блокирует отправителя при заполнении — старые
/// неприсланные сообщения у медленного подписчика просто отбрасываются
/// (`RecvError::Lagged`), новые продолжают прибывать; так и должно быть
/// здесь, см. документацию `subscribe_logs` за тем, почему это
/// предпочтительнее, чем застопорить чтение stdout/stderr самого QEMU.
/// Значение — компромисс между памятью (один буфер на каждый живой
/// инстанс, даже без подписчиков) и тем, сколько недавних строк успеет
/// получить клиент, подключившийся через долю секунды после момента, как
/// они были написаны, прежде чем более старые из них вытеснятся; не
/// предназначено как история для уже отключённого клиента (live-tail —
/// явное решение первой версии, см. `andler_core::HypervisorBackend::log_stream`).
const LOG_CHANNEL_CAPACITY: usize = 256;

/// Емкость `broadcast`-канала метрик ресурсов (см.
/// `QemuProcess::metrics_sender`/`subscribe_metrics`).
/// Метрики публикуются раз в ~1 секунду (см.
/// `andler_qemu::metrics::DEFAULT_POLL_INTERVAL`), 64 выборки — ~1 минута
/// буфера; нового подписчика не должны терять свежие данные.
const METRICS_CHANNEL_CAPACITY: usize = 64;

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
    /// Канал, в который `drain_to_tracing` дублирует каждую прочитанную
    /// строку stdout/stderr, помимо записи в `tracing` — источник для
    /// `subscribe_logs`/`andler_core::HypervisorBackend::log_stream`.
    /// `broadcast`, не `mpsc`: несколько клиентов могут одновременно
    /// стримить логи одного инстанса (см. документацию `subscribe_logs`),
    /// `mpsc` отдал бы каждую строку только одному из них. Хранится здесь,
    /// не отдельно от `Child` — обе части умирают вместе с самим
    /// `QemuProcess` (см. документацию `backend::RunningInstance`, где
    /// живёт сам `QemuProcess`).
    log_sender: broadcast::Sender<LogLine>,
    /// Канал метрик ресурсов — публикуется раз в секунду фоновой задачей
    /// `spawn_metrics_poller` (см. `andler_qemu::metrics`). Аналогичен
    /// `log_sender` по паттерну: несколько gRPC-клиентов подписываются
    /// одновременно, broadcast обеспечивает fan-out.
    metrics_sender: broadcast::Sender<ResourceMetrics>,
    /// Хэндл фоновой задачи поллинга метрик — хранится здесь, чтобы
    /// `JoinHandle` не был drop'нут ( drop отменяет задачу в tokio); сам
    /// `.await` на нём никогда не вызывается — `QemuProcess` живёт столько
    /// же, сколько и поллер.
    _metrics_task: tokio::task::JoinHandle<()>,
}

impl QemuProcess {
    /// Запускает `qemu-system-x86_64` с заданными аргументами (обычно —
    /// результат `cmdline::build_args`).
    ///
    /// stdout/stderr процесса перенаправляются в `Stdio::piped()`, а не
    /// наследуются от `andlerd` — иначе вывод множества QEMU-инстансов
    /// смешивался бы в одном терминале демона. Оба потока вычитываются
    /// построчно в фоновых задачах, логируются через `tracing::warn!` и
    /// публикуются в broadcast-канал для `subscribe_logs` (см.
    /// `drain_to_tracing` ниже) — раньше дескрипторы просто никем не
    /// читались, и причину падения процесса узнать было невозможно без
    /// внешних средств.
    pub async fn spawn(args: &[String], qmp_socket_path: PathBuf) -> Result<Self, ProcessError> {
        let mut child = Command::new(QEMU_BINARY)
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

        // Раньше вывод процесса никем не читался (см. историю этого
        // комментария в README крейта) — pid живого процесса было видно,
        // но причину падения (например, неподходящий аргумент командной
        // строки, отсутствующий файл прошивки, недоступный /dev/kvm)
        // узнать было невозможно без внешних средств. Здесь — минимальное
        // решение: вычитываем stdout/stderr построчно в фоновых задачах и
        // логируем через `tracing`, привязывая к pid. Не блокирует
        // основной поток управления и не задерживает возврат из `spawn`;
        // если процесс пишет в stderr быстрее, чем мы читаем, буфер ОС
        // всё равно ограничен — но теперь хотя бы то, что было прочитано,
        // не пропадает молча.
        //
        // Те же строки дублируются в `log_sender` (см. документацию поля)
        // для `subscribe_logs`/`log_stream` — `tracing` остаётся
        // источником для оператора демона (журнал процесса), `log_sender`
        // — для клиента поверх gRPC; `drain_to_tracing` пишет в оба места
        // за один проход по строке, не заводя двух читателей одного
        // потока (что и невозможно — `ChildStdout`/`ChildStderr` можно
        // прочитать только один раз).
        let (log_sender, _) = broadcast::channel(LOG_CHANNEL_CAPACITY);

        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(Self::drain_to_tracing(
                stdout,
                pid,
                LogStreamSource::Stdout,
                log_sender.clone(),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(Self::drain_to_tracing(
                stderr,
                pid,
                LogStreamSource::Stderr,
                log_sender.clone(),
            ));
        }

        // Фоновая задача поллинга метрик из /proc/<pid>/ — аналогична
        // drain_to_tracing по жизненному циклу: живёт столько же, сколько
        // и QemuProcess, автоматически завершается при его завершении.
        let (metrics_sender, _) = broadcast::channel(METRICS_CHANNEL_CAPACITY);
        let ticks_per_sec = crate::metrics::ticks_per_second();
        let metrics_task = crate::metrics::spawn_metrics_poller(
            pid,
            ticks_per_sec,
            crate::metrics::DEFAULT_POLL_INTERVAL,
            metrics_sender.clone(),
        );

        Ok(QemuProcess {
            child,
            pid,
            qmp_socket_path,
            log_sender,
            metrics_sender,
            _metrics_task: metrics_task,
        })
    }

    /// Построчно читает `reader` до EOF, логирует каждую строку через
    /// `tracing::warn!` (не `info!` — вывод QEMU в норме почти пуст;
    /// что-то в нём появляющееся обычно стоит внимания при диагностике,
    /// даже если сам процесс в итоге работает штатно) и одновременно
    /// публикует её в `sender` для подписчиков `subscribe_logs`. `source`
    /// — `LogStreamSource::Stdout`/`Stderr`, чтобы не путать источники.
    ///
    /// Ошибка `send` (нет подписчиков) намеренно проигнорирована — это
    /// штатный случай: в любой момент может не быть ни одного активного
    /// gRPC-клиента, стримящего логи, и строка просто никому не нужна
    /// прямо сейчас; `tracing` уже получил её строкой выше независимо от
    /// этого.
    async fn drain_to_tracing<R>(
        reader: R,
        pid: u32,
        source: LogStreamSource,
        sender: broadcast::Sender<LogLine>,
    )
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let stream_name = match source {
            LogStreamSource::Stdout => "stdout",
            LogStreamSource::Stderr => "stderr",
        };
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    tracing::warn!(pid, stream = stream_name, "{line}");
                    let _ = sender.send(LogLine { source, line });
                }
                Ok(None) => break,
                Err(io_err) => {
                    tracing::warn!(pid, stream = stream_name, error = %io_err, "failed to read qemu output");
                    break;
                }
            }
        }
    }

    /// Подписывает нового слушателя на live-tail stdout/stderr этого
    /// процесса (см. документацию `log_sender`).
    ///
    /// Новый подписчик получает только строки, отправленные *после*
    /// вызова `subscribe_logs` — `broadcast::Sender::subscribe()` не
    /// отдаёт историю, см. документацию
    /// `andler_core::HypervisorBackend::log_stream` за тем, что это
    /// сознательный выбор первой версии, не упущение.
    pub fn subscribe_logs(&self) -> broadcast::Receiver<LogLine> {
        self.log_sender.subscribe()
    }

    /// Подписывает нового слушателя на live-tail метрик ресурсов этого
    /// процесса (см. документацию `metrics_sender`).
    ///
    /// Новый подписчик получает только выборки, опубликованные *после*
    /// вызова `subscribe_metrics` — `broadcast::Sender::subscribe()` не
    /// отдаёт историю (аналогично `subscribe_logs`).
    pub fn subscribe_metrics(&self) -> broadcast::Receiver<ResourceMetrics> {
        self.metrics_sender.subscribe()
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

    /// Проверяет `drain_to_tracing` напрямую на `io::Cursor` (не на
    /// реальном `ChildStdout`) — не требует бинарника QEMU, безопасно
    /// гонять в unit-test. Подписчик, созданный *до* того, как
    /// `drain_to_tracing` начал читать, должен получить обе строки в
    /// порядке появления.
    #[tokio::test]
    async fn drain_to_tracing_publishes_lines_to_subscriber() {
        let (sender, mut receiver) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        let reader: &[u8] = b"first line\nsecond line\n";

        QemuProcess::drain_to_tracing(reader, 1234, LogStreamSource::Stdout, sender).await;

        let first = receiver.try_recv().expect("first line should be queued");
        assert_eq!(first.source, LogStreamSource::Stdout);
        assert_eq!(first.line, "first line");

        let second = receiver.try_recv().expect("second line should be queued");
        assert_eq!(second.source, LogStreamSource::Stdout);
        assert_eq!(second.line, "second line");

        assert!(receiver.try_recv().is_err(), "no more lines after EOF");
    }

    /// `drain_to_tracing` должен не падать и не блокироваться, если ни
    /// один подписчик не существует (`sender.send` возвращает `Err`,
    /// который намеренно игнорируется) — см. документацию
    /// `drain_to_tracing` за тем, почему это штатный случай, не ошибка.
    #[tokio::test]
    async fn drain_to_tracing_tolerates_no_subscribers() {
        let (sender, _) = broadcast::channel::<LogLine>(LOG_CHANNEL_CAPACITY);
        let reader: &[u8] = b"nobody is listening\n";

        // Не должно ни паниковать, ни зависнуть — если бы здесь была
        // ошибка обработки `send`, тест завис бы или упал бы.
        QemuProcess::drain_to_tracing(reader, 1, LogStreamSource::Stderr, sender).await;
    }

    /// Несколько подписчиков, оформленных через одинаковый `Sender`
    /// (модель `subscribe_logs` — см. его документацию), должны получить
    /// одну и ту же строку независимо друг от друга — обоснование того,
    /// почему канал `broadcast`, а не `mpsc`.
    #[tokio::test]
    async fn multiple_subscribers_each_receive_the_same_line() {
        let (sender, mut first_receiver) = broadcast::channel(LOG_CHANNEL_CAPACITY);
        let mut second_receiver = sender.subscribe();
        let reader: &[u8] = b"shared line\n";

        QemuProcess::drain_to_tracing(reader, 1, LogStreamSource::Stdout, sender).await;

        assert_eq!(
            first_receiver.try_recv().unwrap().line,
            "shared line".to_string()
        );
        assert_eq!(
            second_receiver.try_recv().unwrap().line,
            "shared line".to_string()
        );
    }
}
