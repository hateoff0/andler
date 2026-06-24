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
use andler_rpc::proto::{
    AudioConfig, CpuConfig, CreateInstanceRequest, DiskConfig, DisplayConfig, FirmwareConfig,
    GpuConfig, InputConfig, InstanceIdRequest, InstanceStateKind, MemoryConfig, NetworkConfig,
    Resolution,
};
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

/// Полный набор полей `CreateInstanceRequest`, соответствующий
/// `andler_core::config::*::reference_default()` — используется как
/// валидная база для теста ниже и для модификаций в негативных сценариях.
/// Живёт здесь (а не в `andler-rpc::convert::tests`, где уже есть похожий
/// `sample_instance_config`), потому что строит именно `proto`-сообщение
/// напрямую, без прохода через `InstanceConfig` — этот тест проверяет
/// сериализацию через реальный TCP, а не саму конвертацию (та уже покрыта
/// `andler-rpc::convert::tests::create_instance_request_round_trips_into_instance_config`
/// и соседними тестами).
fn sample_create_instance_request() -> CreateInstanceRequest {
    use andler_rpc::proto::{network_mode, render_backend};

    let mut cpu = CpuConfig {
        cores: 4,
        sockets: 1,
        threads: 1,
        affinity: vec![],
        ..Default::default()
    };
    cpu.set_priority(andler_rpc::proto::CpuPriority::Normal);

    let mut disk = DiskConfig {
        path: "/tmp/disk.qcow2".to_string(),
        size_bytes: 40 * 1024 * 1024 * 1024,
        base_image: String::new(),
        thin_provisioning: true,
        trim_on_shutdown: true,
        ..Default::default()
    };
    disk.set_format(andler_rpc::proto::DiskFormat::Qcow2);

    let mut display = DisplayConfig {
        resolution: Some(Resolution {
            width: 1920,
            height: 1080,
        }),
        dpi: 96,
        fps_limit: 0,
        fullscreen: false,
        ..Default::default()
    };
    display.set_display_engine(andler_rpc::proto::DisplayEngine::Sdl);

    let gpu = GpuConfig {
        render_backend: Some(andler_rpc::proto::RenderBackend {
            kind: Some(render_backend::Kind::Venus(render_backend::Venus {})),
        }),
        hostmem_bytes: 4096 * 1024 * 1024,
        blob: true,
        gl: true,
    };

    let network = NetworkConfig {
        mode: Some(andler_rpc::proto::NetworkMode {
            kind: Some(network_mode::Kind::Nat(network_mode::Nat {})),
        }),
        device_model: "virtio-net-pci".to_string(),
    };

    let firmware = FirmwareConfig {
        ovmf_code_path: "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd".to_string(),
        ovmf_vars_path: "/tmp/test_VARS.fd".to_string(),
    };

    let mut audio = AudioConfig::default();
    audio.set_backend(andler_rpc::proto::AudioBackend::Pipewire);

    let input = InputConfig {
        tablet_mode: true,
        hide_host_cursor: true,
        clipboard_enabled: true,
    };

    CreateInstanceRequest {
        name: "test-linux-vm".to_string(),
        iso_path: "/tmp/test.iso".to_string(),
        cpu: Some(cpu),
        memory: Some(MemoryConfig {
            size_bytes: 8 * 1024 * 1024 * 1024,
            ballooning: false,
            zram: false,
            ksm: true,
        }),
        disk: Some(disk),
        display: Some(display),
        gpu: Some(gpu),
        network: Some(network),
        firmware: Some(firmware),
        audio: Some(audio),
        input: Some(input),
    }
}

#[tokio::test]
async fn create_instance_round_trips_over_real_grpc_and_status_reports_created() {
    // Самый рискованный путь этой партии изменений: `RenderBackend`/
    // `NetworkMode` — `oneof`, и единственный способ убедиться, что
    // `prost` реально (де)сериализует их так, как ожидает `convert.rs`
    // (а не, например, теряет ветку при кодировании через настоящий
    // protobuf wire format, в отличие от прямого вызова конвертации в
    // памяти, как делают unit-тесты `andler-rpc::convert::tests`) — это
    // прогнать запрос через настоящий TCP-сокет, как здесь.
    let (mut client, server) = spawn_server_and_connect().await;

    let response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("a fully-populated CreateInstanceRequest must be accepted")
        .into_inner();

    let id = uuid::Uuid::parse_str(&response.instance_id)
        .expect("CreateInstanceResponse.instance_id must be a valid UUID");

    let status = client
        .get_instance_status(InstanceIdRequest {
            instance_id: id.to_string(),
        })
        .await
        .expect("freshly created instance must be found")
        .into_inner();
    assert_eq!(status.state, InstanceStateKind::Created as i32);

    server.abort();
}

#[tokio::test]
async fn create_instance_missing_cpu_field_round_trips_as_invalid_argument() {
    let (mut client, server) = spawn_server_and_connect().await;

    let mut request = sample_create_instance_request();
    request.cpu = None;

    let status = client
        .create_instance(request)
        .await
        .expect_err("a CreateInstanceRequest missing a required field must be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn create_instance_with_bridge_network_round_trips_over_real_grpc() {
    // `NetworkMode::Bridge` несёт данные (`interface`), в отличие от
    // `Nat`/`Isolated` — отдельный тест проверяет, что oneof-ветка с
    // полем, не только маркерные пустые message'и, переживает настоящую
    // protobuf-сериализацию по TCP.
    use andler_rpc::proto::network_mode;

    let (mut client, server) = spawn_server_and_connect().await;
    let mut request = sample_create_instance_request();
    request.network = Some(NetworkConfig {
        mode: Some(andler_rpc::proto::NetworkMode {
            kind: Some(network_mode::Kind::Bridge(network_mode::Bridge {
                interface: "br0".to_string(),
            })),
        }),
        device_model: "virtio-net-pci".to_string(),
    });

    let response = client
        .create_instance(request)
        .await
        .expect("a request with NetworkMode::Bridge must be accepted")
        .into_inner();
    uuid::Uuid::parse_str(&response.instance_id)
        .expect("CreateInstanceResponse.instance_id must be a valid UUID");

    server.abort();
}
