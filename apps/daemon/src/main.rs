mod daemon;
mod firmware;
mod log_ring;
mod metrics;
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

    let dev_restart = std::env::var("ANDLERD_DEV_RESTART")
        .map(|value| value == "1")
        .unwrap_or(false);
    if dev_restart {
        tracing::info!("ANDLERD_DEV_RESTART=1: leaving VMs running for daemon restart");
        return;
    }

    let running_ids: Vec<andler_core::InstanceId> = {
        let supervisors = daemon.supervisors.read().await;
        supervisors
            .iter()
            .filter(|(_, handle)| {
                matches!(
                    handle.state(),
                    andler_core::InstanceState::Running | andler_core::InstanceState::Paused
                )
            })
            .map(|(id, _)| *id)
            .collect()
    };

    for id in running_ids {
        tracing::info!(instance_id = %id, "stopping instance on shutdown");
        if let Err(error) = daemon.stop_instance(id, true).await {
            tracing::warn!(
                instance_id = %id,
                %error,
                "failed to stop instance on shutdown"
            );
        }
    }

    tracing::info!("graceful shutdown complete");
}

const ANDLER_CRATE_TARGETS: &[&str] = &[
    "daemon",
    "andler_core",
    "andler_qemu",
    "andler_firmware",
    "andler_store",
    "andler_disk",
    "andler_rpc",
];

fn verbosity_from_args() -> u8 {
    verbosity_from(std::env::args().skip(1))
}

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

fn init_tracing() -> std::sync::Arc<log_ring::LogRing> {
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

    let ring = log_ring::LogRing::new();
    let writer = log_ring::RingMakeWriter::new(ring.clone());
    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(writer)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .init();
    }
    ring
}

fn default_store_path() -> String {
    andler_core::paths::db_path().to_string_lossy().into_owned()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ring = init_tracing();

    // One daemon per ANDLER_HOME: the lock file lives next to the database
    // and is held (via the File's fd) for the whole process lifetime.
    let home = andler_core::paths::andler_home();
    std::fs::create_dir_all(&home)?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(home.join("andlerd.lock"))?;
    use std::os::fd::AsRawFd;
    // SAFETY: fd is a valid open file descriptor owned by lock_file; flock
    // does not take ownership and only fails with an errno we inspect.
    let lock_result = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if lock_result != 0 {
        eprintln!(
            "andlerd: another daemon is already running for {} (flock failed: {}); \
             stop it or point ANDLER_HOME elsewhere",
            home.display(),
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }
    let _lock_file = lock_file;

    let addr: SocketAddr = std::env::var("ANDLERD_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string())
        .parse()?;
    let store_path = std::env::var("ANDLERD_STORE_PATH").unwrap_or_else(|_| default_store_path());

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
    let daemon_metrics = std::sync::Arc::new(metrics::DaemonMetrics::default());
    let service = DaemonService::new(Arc::clone(&daemon), ovmf, ring, daemon_metrics.clone());

    spawn_health_check_task(Arc::clone(&daemon));

    println!("andlerd: listening on {addr}");

    Server::builder()
        .layer(metrics::MetricsLayer::new(daemon_metrics))
        .add_service(AndlerServiceServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal(daemon))
        .await?;

    Ok(())
}

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
        ticker.tick().await;
        loop {
            ticker.tick().await;
            daemon.run_health_check_once().await;
        }
    });
}
