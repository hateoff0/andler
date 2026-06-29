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
//! умолчанию `andlerd-state.db` в текущем рабочем каталоге процесса (а не
//! абсолютный `/var/lib/andler/state.db` из архитектурного плана — выбор
//! и проверка прав на запись в системный каталог при первом запуске
//! остаются для packaging-шага, не для самого бинарника на этом этапе
//! разработки). При старте `andlerd` восстанавливает все ранее
//! сохранённые инстансы через `Daemon::restore` — см. документацию этого
//! метода про то, почему нетерминальные состояния (`Running` и т.п.)
//! восстанавливаются как `Error`, а не как есть.

mod daemon;
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
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~/.local/share"))
        .join("andler/state.db")
        .to_string_lossy()
        .into_owned()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `tracing_subscriber::fmt` — минимальный вывод в stderr с уровнем по
    // умолчанию `info` (переопределяется `RUST_LOG`, стандартное поведение
    // `EnvFilter`). Без подписчика `tracing::warn!`/`tracing::error!` в
    // `daemon.rs` (см. `persist_state`/`persist_new_instance`/`restore`)
    // никуда не выводились бы — это первое использование `tracing` в
    // проекте, до сих пор было достаточно `println!`.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let addr: SocketAddr = std::env::var("ANDLERD_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string())
        .parse()?;
    let store_path =
        std::env::var("ANDLERD_STORE_PATH").unwrap_or_else(|_| default_store_path());

    if let Some(parent) = std::path::Path::new(&store_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Store::open(&store_path).await?;
    let daemon = Daemon::restore(store).await?;
    tracing::info!(store_path = %store_path, "restored instances from store");

    let daemon = Arc::new(daemon);
    let service = DaemonService::new(daemon);

    println!("andlerd: listening on {addr}");

    Server::builder()
        .add_service(AndlerServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
