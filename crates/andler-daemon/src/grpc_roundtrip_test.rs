//! Сквозной тест gRPC-слоя.
//!
//! `andler-rpc::convert` уже проверен unit-тестами (proto<->domain
//! конвертации), но ни один тест до этого момента не поднимал настоящий
//! `tonic::transport::Server` и не гонял запрос через реальный TCP-сокет —
//! то есть весь путь "клиент сериализует запрос -> сервер десериализует,
//! роутит на нужный метод трейта, зовёт `Daemon`, сериализует ответ
//! обратно" был проверен только компиляцией, не поведением. Этот файл
//! закрывает именно это: реальный `AndlerServiceServer` на эфемерном
//! localhost-порту, реальный `AndlerServiceClient` через `tonic::Channel`.
//!
//! Без диска/QEMU (это не интеграционный тест в смысле `docker/README.md`
//! — ему не нужен `qemu-img`/`/dev/kvm`, только TCP-loopback), поэтому
//! гоняется как обычный unit-тест, не помечен `#[ignore]`.

use std::sync::Arc;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::andler_service_server::AndlerServiceServer;
use andler_rpc::proto::InstanceIdRequest;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::daemon::Daemon;
use crate::service::DaemonService;

/// Поднимает `DaemonService` на свежем `Daemon` на эфемерном
/// `127.0.0.1`-порту (выбранном ОС через `:0`, без гонки за порт между
/// параллельными тестами) и возвращает подключённый клиент плюс хэндл
/// сервера (чтобы вызывающий мог его `abort()`-нуть в конце теста — у
/// `Server::serve_with_incoming` нет естественного способа остановиться,
/// кроме отмены задачи, в которой он работает).
async fn spawn_server_and_connect() -> (
    AndlerServiceClient<tonic::transport::Channel>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral loopback port must not fail in a test sandbox");
    let addr = listener
        .local_addr()
        .expect("a just-bound listener must have a local address");

    let daemon = Arc::new(Daemon::new());
    let service = DaemonService::new(daemon);

    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(AndlerServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("serve_with_incoming must not fail for a healthy listener");
    });

    // `listener` уже забинден (и его backlog уже принимает соединения на
    // уровне ОС) до того, как accept-loop внутри `server`-таска реально
    // начал крутиться — `connect` может блокироваться до первого `accept`,
    // но не должен фейлиться race condition'ом из-за порядка планирования
    // задач tokio.
    let client = AndlerServiceClient::connect(format!("http://{addr}"))
        .await
        .expect("client must be able to connect to the just-spawned server");

    (client, server)
}

#[tokio::test]
async fn unknown_instance_round_trips_as_not_found_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;
    let unknown_id = uuid::Uuid::new_v4().to_string();

    // Если бы gRPC-слой был сломан (неверный путь метода, поломанная
    // (де)сериализация protobuf, паника внутри сервиса) — мы получили бы
    // транспортную ошибку (`tonic::Status` с кодом `Unknown`/`Internal`
    // от самого tonic, не от `Daemon`) или вообще обрыв соединения, а не
    // предсказуемый `NOT_FOUND`, который реально возвращает `Daemon`.
    // Этот тест проверяет именно то, что unit-тесты `convert.rs` не могут:
    // что весь путь туда и обратно по сети действительно работает.
    let status = client
        .get_instance_status(InstanceIdRequest {
            instance_id: unknown_id.clone(),
        })
        .await
        .expect_err("unknown instance must be reported as a gRPC error, not succeed");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .start_instance(InstanceIdRequest {
            instance_id: unknown_id.clone(),
        })
        .await
        .expect_err("starting an unknown instance must round-trip as NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .pause_instance(InstanceIdRequest {
            instance_id: unknown_id.clone(),
        })
        .await
        .expect_err("pausing an unknown instance must round-trip as NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .resume_instance(InstanceIdRequest {
            instance_id: unknown_id,
        })
        .await
        .expect_err("resuming an unknown instance must round-trip as NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn malformed_instance_id_round_trips_as_invalid_argument_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    // `parse_instance_id` (andler-rpc::convert) отклоняет всё, что не
    // парсится как UUID, через `ConvertError -> Status::invalid_argument`
    // (см. impl в convert.rs, добавленный именно из-за orphan rule).
    // Этот тест проверяет, что данный путь конвертации действительно
    // подключён к gRPC-сервису, а не просто существует и компилируется.
    let status = client
        .get_instance_status(InstanceIdRequest {
            instance_id: "not-a-uuid".to_string(),
        })
        .await
        .expect_err("malformed instance_id must be rejected, not panic the service");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let status = client
        .get_instance_status(InstanceIdRequest {
            instance_id: String::new(),
        })
        .await
        .expect_err("empty instance_id must be rejected as INVALID_ARGUMENT too");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}
