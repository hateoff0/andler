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

mod daemon;
mod service;

#[cfg(test)]
mod grpc_roundtrip_test;

use std::net::SocketAddr;
use std::sync::Arc;

use andler_rpc::proto::andler_service_server::AndlerServiceServer;
use daemon::Daemon;
use service::DaemonService;
use tonic::transport::Server;

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:50051";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::var("ANDLERD_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string())
        .parse()?;

    let daemon = Arc::new(Daemon::new());
    let service = DaemonService::new(daemon);

    println!("andlerd: listening on {addr}");

    Server::builder()
        .add_service(AndlerServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
