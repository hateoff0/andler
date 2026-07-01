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

fn default_store_path() -> String {
    andler_core::paths::db_path().to_string_lossy().into_owned()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

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
        std::fs::create_dir_all(parent)?;
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
    let service = DaemonService::new(daemon, ovmf);

    println!("andlerd: listening on {addr}");

    Server::builder()
        .add_service(AndlerServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
