//! Бинарник `andlerd`.
//!
//! Реальный долгоживущий gRPC-сервер: `DaemonService` (`service.rs`) как
//! тонкая обёртка над `Daemon` (`daemon.rs`), поднятая через
//! `tonic::transport::Server`. Адрес слушателя — `ANDLERD_LISTEN_ADDR`,
//! по умолчанию `127.0.0.1:50051`: дефолт на loopback, а не `0.0.0.0`,
//! осознанно — andlerd пока не имеет аутентификации/TLS (см. открытый
//! вопрос в README этого крейта про non-goals на этом этапе), так что
//! слушать снаружи loopback по умолчанию означало бы открыть
//! неавторизованный контроль над VM на любом интерфейсе без явного opt-in.
//!
//! Путь к sqlite-файлу персистентности — `ANDLERD_STORE_PATH`, по
//! умолчанию `andler_core::paths::db_path()` (`<ANDLER_HOME>/andlerd.db`,
//! см. PLAN.md, раздел «Структура хранения» — единая точка резолва
//! путей). Раньше дефолтом был `andlerd-state.db` в текущем рабочем
//! каталоге процесса; это было удобно для разработки, но не совпадало с
//! задокументированной структурой хранения ANDLER и ломалось при запуске
//! daemon'а не из той директории, откуда его обычно запускают. Выбор и
//! проверка прав на запись в системный каталог (`/var/lib/andler/...`)
//! для packaging-сценария остаются отдельной задачей, не входят в этот
//! бинарник на текущем этапе разработки. При старте `andlerd` восстанавливает все ранее
//! сохранённые инстансы через `Daemon::restore` — см. документацию этого
//! метода про то, почему нетерминальные состояния (`Running` и т.п.)
//! восстанавливаются как `Error`, а не как есть.
//!
//! Логирование — `info` по умолчанию, `-v`/`--verbose` поднимает
//! andler-крейты (не зависимости вроде `tonic`) до `debug`, `-vv`/
//! `--trace` — до `trace`; `RUST_LOG`, если задан, имеет приоритет над
//! обоими флагами целиком (см. `init_tracing`/`verbosity_from_args` ниже
//! и PLAN.md, раздел "Logging"). `ANDLERD_LOG_FORMAT=json` переключает
//! вывод на JSON-lines вместо человекочитаемого текста (тоже см.
//! `init_tracing`). Логи самого QEMU-процесса отдельного инстанса
//! пишутся в `<instances_root>/<id>/qemu.log`, не смешиваясь с этим
//! потоком — см. `andler_qemu::process::QemuProcess::spawn`.

mod daemon;
mod firmware;
mod service;

#[cfg(test)]
mod grpc_roundtrip_test;

use std::net::SocketAddr;
use std::sync::Arc;

use andler_rpc::proto::andler_service_server::AndlerServiceServer;
use andler_store::Store;
use daemon::Daemon;
use service::DaemonService;
use tonic::transport::Server;

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:50051";

/// Waits for SIGINT (Ctrl+C) or SIGTERM (`systemctl stop`/`kill`), then
/// gracefully stops every instance still `Running`/`Paused` before
/// returning — used as the shutdown future for
/// `Server::serve_with_shutdown` below, so the gRPC server itself only
/// actually stops accepting connections/finishes in-flight requests
/// *after* this completes.
///
/// Before this existed, killing `andlerd` (Ctrl+C or `systemctl stop`,
/// now that item 9 adds a systemd unit) left every running QEMU process
/// as an orphan — `Child`'s `kill_on_drop(true)` (see
/// `andler_qemu::process::QemuProcess`) only fires when the `Child`
/// value itself is dropped, and an ungracefully-killed `andlerd` process
/// doesn't run any Rust destructors at all, so `kill_on_drop` never gets
/// the chance to act. On next startup those orphaned instances were
/// marked `Error` (see `Daemon::restore`) — correct in that it doesn't
/// lie about being able to control them, but the QEMU processes
/// themselves kept running unmanaged in the background instead of
/// actually being asked to shut down. See PLAN.md, item 10, "Signal
/// handling and graceful shutdown".
///
/// SIGKILL (`kill -9`) still can't be caught by any process, Rust or
/// otherwise — this only improves the SIGINT/SIGTERM case, which covers
/// Ctrl+C and every normal `systemctl stop`/`kill`/`kill -TERM`.
async fn shutdown_signal(daemon: Arc<Daemon>) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl+C handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received, stopping running instances gracefully");

    let running_ids: Vec<andler_core::InstanceId> = {
        let instances = daemon.instances.read().await;
        instances
            .iter()
            .filter(|(_, record)| {
                matches!(
                    record.state,
                    andler_core::InstanceState::Running | andler_core::InstanceState::Paused
                )
            })
            .map(|(id, _)| *id)
            .collect()
    };

    for id in running_ids {
        tracing::info!(instance_id = %id.0, "stopping instance on shutdown");
        if let Err(error) = daemon.stop_instance(id, true).await {
            tracing::warn!(
                instance_id = %id.0,
                %error,
                "failed to stop instance on shutdown"
            );
        }
    }

    tracing::info!("graceful shutdown complete");
}

/// Andler-специфичные крейты, у которых уровень меняется вместе с
/// `-v`/`-vv` (см. `init_tracing`). Зависимости (`tonic`, `h2`, и т.д.)
/// намеренно не включены — на `debug`/`trace` они генерируют трафик,
/// который не помогает диагностировать сам ANDLER (см. PLAN.md, раздел
/// "Logging": "не заваливать консоль низкоуровневым шумом"), поэтому
/// остаются на общем дефолте `info` даже при `-vv`.
const ANDLER_CRATE_TARGETS: &[&str] = &[
    "daemon",
    "andler_core",
    "andler_qemu",
    "andler_firmware",
    "andler_store",
    "andler_disk",
    "andler_rpc",
];

/// Считает уровень verbosity из argv: 0 (дефолт) = `info`, 1
/// (`-v`/`--verbose`) = `debug` (полная командная строка QEMU,
/// промежуточные шаги — поиск OVMF, резолв путей), 2+ (`-vv`) = `trace`
/// (QMP-протокол целиком, каждый вызов backend-метода). См. PLAN.md,
/// раздел "Logging", таблицу уровней. Повторные флаги суммируются и
/// ограничиваются сверху `trace` (`-vvv` не даёт уровня выше `trace`,
/// такого уровня в этой схеме просто нет).
///
/// Единственный минимальный парсер здесь, не `clap`, — `andlerd` уже
/// сплошь настраивается через переменные окружения (`ANDLERD_*`, см.
/// комментарий в начале файла), а не через флаги; заводить полноценный
/// CLI-парсер ради одного флага было бы непропорционально, а
/// неизвестные аргументы (кроме перечисленных ниже) намеренно
/// игнорируются, а не отклоняются как ошибка.
fn verbosity_from_args() -> u8 {
    verbosity_from(std::env::args().skip(1))
}

/// Чистая часть `verbosity_from_args`, принимающая произвольный итератор
/// строк вместо `std::env::args()` — так `-v`/`-vv`-парсинг тестируется
/// юнит-тестами без реального запуска процесса с разными argv.
fn verbosity_from<I, S>(args: I) -> u8
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut level: u8 = 0;
    for arg in args {
        level += match arg.as_ref() {
            "-v" | "--verbose" | "--debug" => 1,
            "-vv" | "--trace" => 2,
            _ => 0,
        };
    }
    level.min(2)
}

#[cfg(test)]
mod verbosity_tests {
    use super::verbosity_from;

    #[test]
    fn no_flags_is_zero() {
        assert_eq!(verbosity_from(Vec::<&str>::new()), 0);
    }

    #[test]
    fn single_v_is_one() {
        assert_eq!(verbosity_from(["-v"]), 1);
        assert_eq!(verbosity_from(["--verbose"]), 1);
        assert_eq!(verbosity_from(["--debug"]), 1);
    }

    #[test]
    fn double_v_flag_is_two() {
        assert_eq!(verbosity_from(["-vv"]), 2);
        assert_eq!(verbosity_from(["--trace"]), 2);
    }

    #[test]
    fn repeated_single_v_flags_add_up() {
        assert_eq!(verbosity_from(["-v", "-v"]), 2);
    }

    #[test]
    fn level_is_capped_at_trace() {
        assert_eq!(verbosity_from(["-vv", "-v", "-v"]), 2);
    }

    #[test]
    fn unrelated_args_are_ignored() {
        assert_eq!(verbosity_from(["--name", "my-vm", "-v"]), 1);
    }
}

/// Инициализирует `tracing_subscriber`. `RUST_LOG`, если задан, имеет
/// приоритет над `-v`/`-vv` целиком (тот же принцип, что и у остальной
/// конфигурации `andlerd` через `ANDLERD_*` — явно заданное окружением
/// значение не переопределяется флагом умолчания) — так и раньше себя
/// вела `EnvFilter::from_default_env()`, здесь это сохранено явно, а не
/// потеряно при добавлении `-v`/`-vv`.
///
/// `ANDLERD_LOG_FORMAT=json` переключает вывод на JSON-lines (одна
/// JSON-запись на строку, через `tracing_subscriber`'s `.json()`-layer;
/// см. PLAN.md, раздел "Logging", "Структурированный формат для daemon")
/// — удобно для агрегации логов нескольких инстансов `andlerd` или для
/// внешних систем мониторинга. Любое другое значение (включая
/// отсутствие переменной) — текущий человекочитаемый `fmt()`-вывод, тот
/// же, что и раньше; это дефолт, а не `json`, потому что `andlerd` чаще
/// всего запускается вручную во время разработки, где читаемый вывод в
/// терминале удобнее необработанного JSON построчно.
///
/// `.json()` и обычный `fmt()` — разные типы `SubscriberBuilder`
/// (разный формат полей), поэтому итоговый `if`/`else` с двумя `.init()`
/// не сводится к одной ветке без `Box<dyn ...>` — сам фильтр (`filter`)
/// вычисляется один раз выше, чтобы не дублировать логику выбора
/// уровня, а не сам вызов `.init()`.
fn init_tracing() {
    let json = std::env::var("ANDLERD_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let filter = if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        match verbosity_from_args() {
            0 => tracing_subscriber::EnvFilter::new("info"),
            verbosity => {
                let level = if verbosity == 1 { "debug" } else { "trace" };
                let directive = ANDLER_CRATE_TARGETS
                    .iter()
                    .map(|target| format!("{target}={level}"))
                    .collect::<Vec<_>>()
                    .join(",")
                    + ",info";
                tracing_subscriber::EnvFilter::new(directive)
            }
        }
    };

    if json {
        tracing_subscriber::fmt().json().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

fn default_store_path() -> String {
    andler_core::paths::db_path().to_string_lossy().into_owned()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let addr: SocketAddr = std::env::var("ANDLERD_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string())
        .parse()?;
    let store_path =
        std::env::var("ANDLERD_STORE_PATH").unwrap_or_else(|_| default_store_path());

    // --- OVMF авто-детект ---
    // `ANDLERD_OVMF_CODE` и `ANDLERD_OVMF_VARS` — опциональные переменные
    // окружения для явного переопределения путей к OVMF без пересборки.
    // Если не заданы — запускается `andler_firmware::detect_matched_pair()`,
    // которая ищет OVMF в стандартных системных путях (Arch, Ubuntu,
    // Fedora, openSUSE — см. `andler_firmware::KNOWN_OVMF_CODE_PATHS`).
    // При неудаче daemon завершается с понятной ошибкой, а не при первом
    // `andler create` с непонятным backtrace.
    let ovmf_code = std::env::var("ANDLERD_OVMF_CODE")
        .ok()
        .map(std::path::PathBuf::from);
    let ovmf_vars_template = std::env::var("ANDLERD_OVMF_VARS")
        .ok()
        .map(std::path::PathBuf::from);

    let ovmf = firmware::resolve(ovmf_code, ovmf_vars_template).map_err(|e| {
        eprintln!("andlerd: {e}");
        eprintln!(
            "andlerd: known CODE paths checked:\n{}",
            andler_firmware::KNOWN_OVMF_CODE_PATHS
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        eprintln!(
            "andlerd: known VARS paths checked:\n{}",
            andler_firmware::KNOWN_OVMF_VARS_PATHS
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        e
    })?;

    if let Some(parent) = std::path::Path::new(&store_path).parent() {
        // `0700`, not just default umask — this directory holds
        // `andlerd.db` (every instance's config in one file) plus, by
        // default, `instances/` underneath it. See PLAN.md, item 20b.
        andler_core::paths::ensure_private_dir(parent).await?;
    }

    let store = Store::open(&store_path).await?;
    let daemon = Daemon::restore(store).await?;
    tracing::info!(
        store_path = %store_path,
        ovmf_code = %ovmf.code.display(),
        ovmf_vars_template = %ovmf.vars_template.display(),
        "andlerd started"
    );

    let daemon = Arc::new(daemon);
    let service = DaemonService::new(Arc::clone(&daemon), ovmf);

    spawn_health_check_task(Arc::clone(&daemon));

    println!("andlerd: listening on {addr}");

    Server::builder()
        .add_service(AndlerServiceServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal(daemon))
        .await?;

    Ok(())
}

/// Spawns the background health-check loop (see
/// `daemon::health_ops::run_health_check_once`, ROADMAP.md "Core: add VM
/// health checks and auto-restart on failure"). Interval configurable via
/// `ANDLERD_HEALTH_CHECK_INTERVAL_SECS` (default 30s); set to `0` to
/// disable entirely (e.g. for tests or environments that don't want the
/// extra periodic `backend.status()` calls).
///
/// Not wired into `shutdown_signal`/graceful shutdown — this task holds
/// no state that needs flushing (it only ever reads `daemon.instances`
/// and, on a detected crash, goes through the same `persist_state` path
/// every other state transition already uses), so letting it end
/// abruptly when the process exits is fine; there's nothing to lose.
fn spawn_health_check_task(daemon: Arc<Daemon>) {
    let interval_secs: u64 = std::env::var("ANDLERD_HEALTH_CHECK_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    if interval_secs == 0 {
        tracing::info!("health checks disabled (ANDLERD_HEALTH_CHECK_INTERVAL_SECS=0)");
        return;
    }

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // The first tick fires immediately — skip it so we don't run a
        // health check the instant the daemon starts, before anything
        // has necessarily even been asked to start yet.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            daemon.run_health_check_once().await;
        }
    });
}
