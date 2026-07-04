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
    AudioConfig, CloneInstanceRequest, CloneMode, CpuConfig, CreateInstanceRequest, DiskConfig,
    DisplayConfig, Empty, ExportInstanceDiskRequest, FirmwareConfig, GpuConfig, InputConfig,
    InstanceIdRequest, InstanceStateKind, MemoryConfig, NetworkConfig, RemoveInstanceRequest,
    Resolution,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::daemon::Daemon;
use crate::firmware::OvmfPaths;
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
    let test_ovmf = OvmfPaths {
        code: std::path::PathBuf::from("/usr/share/edk2/x64/OVMF_CODE.4m.fd"),
        vars_template: std::path::PathBuf::from("/usr/share/edk2/x64/OVMF_VARS.4m.fd"),
    };
    let service = DaemonService::new(daemon, test_ovmf);

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
        nat_backend: andler_rpc::proto::NatBackend::Slirp as i32,
    };

    let firmware = FirmwareConfig {
        ovmf_code_path: "/usr/share/edk2/x64/OVMF_CODE.4m.fd".to_string(),
        ovmf_vars_path: "/tmp/test_VARS.fd".to_string(),
    };

    let mut audio = AudioConfig::default();
    audio.set_backend(andler_rpc::proto::AudioBackend::Pipewire);
    audio.set_device(andler_rpc::proto::AudioDevice::VirtioSound);

    let mut input = InputConfig {
        tablet_mode: true,
        hide_host_cursor: true,
        clipboard_enabled: true,
        ..Default::default()
    };
    input.set_pointer_mode(andler_rpc::proto::PointerMode::Tablet);

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
        cdrom_bus: 0, // CdromBus::Ide (default)
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
        nat_backend: andler_rpc::proto::NatBackend::Slirp as i32,
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

#[tokio::test]
async fn list_instances_on_fresh_server_returns_empty() {
    let (mut client, server) = spawn_server_and_connect().await;

    let response = client
        .list_instances(Empty {})
        .await
        .expect("ListInstances on a fresh daemon must succeed")
        .into_inner();
    assert!(response.instances.is_empty());

    server.abort();
}

#[tokio::test]
async fn list_instances_over_real_grpc_reflects_created_instance() {
    // Самое важное здесь не "список не пуст", а то, что он отражает
    // именно тот instance_id, который вернул CreateInstance — без этого
    // ListInstances мог бы технически "работать" и при этом указывать на
    // данные, рассинхронизированные с тем, что реально создал клиент.
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let list_response = client
        .list_instances(Empty {})
        .await
        .expect("list_instances must succeed")
        .into_inner();

    assert_eq!(list_response.instances.len(), 1);
    let entry = &list_response.instances[0];
    assert_eq!(entry.instance_id, create_response.instance_id);
    assert_eq!(entry.name, "test-linux-vm");
    assert_eq!(entry.state, InstanceStateKind::Created as i32);

    server.abort();
}

#[tokio::test]
async fn list_instances_includes_every_created_instance() {
    let (mut client, server) = spawn_server_and_connect().await;

    let mut request_a = sample_create_instance_request();
    request_a.name = "vm-a".to_string();
    let mut request_b = sample_create_instance_request();
    request_b.name = "vm-b".to_string();

    client
        .create_instance(request_a)
        .await
        .expect("creating vm-a must succeed");
    client
        .create_instance(request_b)
        .await
        .expect("creating vm-b must succeed");

    let response = client
        .list_instances(Empty {})
        .await
        .expect("list_instances must succeed")
        .into_inner();

    let mut names: Vec<&str> = response.instances.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["vm-a", "vm-b"]);

    server.abort();
}

#[tokio::test]
async fn remove_instance_over_real_grpc_then_status_returns_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();
    let id = create_response.instance_id;

    client
        .remove_instance(RemoveInstanceRequest {
            instance_id: id.clone(),
            purge: false,
        })
        .await
        .expect("removing a freshly created (Created-state) instance must succeed");

    let status = client
        .get_instance_status(InstanceIdRequest { instance_id: id })
        .await
        .expect_err("a removed instance must no longer be found");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn remove_instance_on_failed_instance_round_trips_over_real_grpc() {
    // RenderBackend::Passthrough гарантированно проваливает spawn на
    // валидации, синхронно и без обращения к реальному
    // qemu-system-x86_64 (см. тот же приём в других тестах этого файла и
    // в daemon::tests) — единственный детерминированный способ получить
    // терминальное Error-состояние через настоящий gRPC-вызов
    // StartInstance, не гадая, есть ли в среде CI рабочий QEMU/KVM.
    use andler_rpc::proto::render_backend;

    let (mut client, server) = spawn_server_and_connect().await;
    let mut request = sample_create_instance_request();
    request.gpu = Some(GpuConfig {
        render_backend: Some(andler_rpc::proto::RenderBackend {
            kind: Some(render_backend::Kind::Passthrough(
                render_backend::Passthrough {
                    gpu_pci_id: "0000:01:00.0".to_string(),
                },
            )),
        }),
        hostmem_bytes: 4096 * 1024 * 1024,
        blob: true,
        gl: true,
    });

    let create_response = client
        .create_instance(request)
        .await
        .expect("create_instance must succeed")
        .into_inner();
    let id = create_response.instance_id;

    client
        .start_instance(InstanceIdRequest {
            instance_id: id.clone(),
        })
        .await
        .expect_err("starting an instance with RenderBackend::Passthrough must fail");

    // Инстанс теперь в Error (терминальное) — remove_instance должен
    // пройти.
    client
        .remove_instance(RemoveInstanceRequest {
            instance_id: id.clone(),
            purge: false,
        })
        .await
        .expect("removing an instance left in Error state must succeed");

    let status = client
        .get_instance_status(InstanceIdRequest { instance_id: id })
        .await
        .expect_err("a removed instance must no longer be found");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn remove_unknown_instance_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .remove_instance(RemoveInstanceRequest {
            instance_id: uuid::Uuid::new_v4().to_string(),
            purge: false,
        })
        .await
        .expect_err("removing an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn get_instance_config_over_real_grpc_returns_what_was_created() {
    // Самое важное здесь — не "ответ не пуст", а то, что конкретные
    // значения, которые клиент передал в CreateInstanceRequest, реально
    // дойдут обратно через GetInstanceConfigResponse по настоящему TCP
    // (включая RenderBackend::Venus, единственный oneof-вариант, который
    // несёт sample_create_instance_request — round-trip всего oneof
    // через protobuf, не только in-memory конвертация).
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();
    let id = create_response.instance_id;

    let config = client
        .get_instance_config(InstanceIdRequest {
            instance_id: id.clone(),
        })
        .await
        .expect("get_instance_config must succeed for a freshly created instance")
        .into_inner();

    assert_eq!(config.instance_id, id);
    assert_eq!(config.name, "test-linux-vm");
    assert_eq!(config.backend(), andler_rpc::proto::BackendKind::Qemu);

    use andler_rpc::proto::instance_kind::Kind;
    match config.kind.expect("kind must be Some").kind {
        Some(Kind::LinuxVm(linux_vm)) => {
            assert_eq!(linux_vm.iso_path, "/tmp/test.iso");
        }
        other => panic!("expected LinuxVm kind, got {other:?}"),
    }

    let gpu = config.gpu.expect("gpu must be Some");
    match gpu.render_backend.expect("render_backend must be Some").kind {
        Some(andler_rpc::proto::render_backend::Kind::Venus(_)) => {}
        other => panic!("expected Venus render backend, got {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn get_instance_config_on_unknown_instance_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .get_instance_config(InstanceIdRequest {
            instance_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .expect_err("get_instance_config on an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

/// Инстанс существует (только что создан через `CreateInstance`), но
/// никогда не запускался — `Daemon::stream_instance_logs` должен отдать
/// пустой, немедленно завершающийся поток (не gRPC-ошибку, см.
/// документацию там). Через настоящий `tonic::Streaming<LogLineResponse>`
/// "немедленно завершающийся" означает, что первый `.message()` уже
/// возвращает `Ok(None)`, а не зависает в ожидании.
#[tokio::test]
async fn stream_instance_logs_for_instance_without_backend_completes_immediately() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();
    let id = create_response.instance_id;

    let mut stream = client
        .stream_instance_logs(InstanceIdRequest { instance_id: id })
        .await
        .expect("streaming logs for an existing, never-started instance must not error")
        .into_inner();

    let first = stream
        .message()
        .await
        .expect("the stream itself must not error");
    assert!(
        first.is_none(),
        "an instance with no running backend handle must yield an empty stream"
    );

    server.abort();
}

#[tokio::test]
async fn stream_instance_logs_on_unknown_instance_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .stream_instance_logs(InstanceIdRequest {
            instance_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .expect_err("streaming logs for an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn stream_instance_logs_rejects_malformed_instance_id() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .stream_instance_logs(InstanceIdRequest {
            instance_id: "not-a-uuid".to_string(),
        })
        .await
        .expect_err("a malformed instance_id must be rejected before reaching Daemon");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn clone_instance_on_unknown_source_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: uuid::Uuid::new_v4().to_string(),
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::Linked as i32,
        })
        .await
        .expect_err("cloning an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

/// Clone LinuxVm с `Linked` требует `qemu-img`. Если `qemu-img`
/// доступен — clone создаёт overlay-диск и succeeds; если нет —
/// возвращает `INTERNAL` (SpawnFailed).
#[tokio::test]
async fn clone_instance_linux_vm_linked_with_qemu_img() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let response = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: create_response.instance_id,
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::Linked as i32,
        })
        .await
        .expect("cloning a LinuxVm with Linked should succeed when qemu-img is available");
    let id = uuid::Uuid::parse_str(&response.into_inner().instance_id)
        .expect("clone response must contain a valid UUID");
    assert!(!id.is_nil());

    server.abort();
}

#[tokio::test]
async fn clone_instance_rejects_unspecified_mode_as_invalid_argument() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    // Намеренно не SharedBaseNotSupportedForLinuxVm: mode конвертируется (и может
    // провалиться) до того, как Daemon::clone_instance успевает увидеть
    // source_instance_id — см. порядок операций в
    // DaemonService::clone_instance (service.rs): parse_instance_id ->
    // CloneMode::try_from -> daemon.clone_instance(...). Здесь источник
    // существует (LinuxVm, тот же, что и в соседнем тесте) специально,
    // чтобы убедиться, что ошибка приходит именно от валидации mode, не
    // от случайного совпадения с "источник не найден"/"LinuxVm + SharedBase".
    let status = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: create_response.instance_id,
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::Unspecified as i32,
        })
        .await
        .expect_err("an unspecified clone mode must be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn clone_instance_rejects_malformed_instance_id() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: "not-a-uuid".to_string(),
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::Linked as i32,
        })
        .await
        .expect_err("a malformed instance_id must be rejected before reaching Daemon");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn export_instance_disk_on_unknown_source_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .export_instance_disk(ExportInstanceDiskRequest {
            source_instance_id: uuid::Uuid::new_v4().to_string(),
            dest_path: "/tmp/export.qcow2".to_string(),
        })
        .await
        .expect_err("exporting an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

/// Export LinuxVm требует `qemu-img`. Если `qemu-img` доступен —
/// export создаёт standalone-файл и succeeds.
#[tokio::test]
async fn export_instance_disk_linux_vm_with_qemu_img() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let response = client
        .export_instance_disk(ExportInstanceDiskRequest {
            source_instance_id: create_response.instance_id,
            dest_path: "/tmp/export.qcow2".to_string(),
        })
        .await
        .expect("exporting a LinuxVm should succeed when qemu-img is available");
    assert!(!response.into_inner().dest_path.is_empty());

    server.abort();
}

/// LinuxVm + FullStandalone — требует `qemu-img` (копирование диска).
/// Если `qemu-img` доступен — clone создаёт standalone-файл и succeeds.
#[tokio::test]
async fn clone_instance_linux_vm_full_standalone_with_qemu_img() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let response = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: create_response.instance_id,
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::FullStandalone as i32,
        })
        .await
        .expect("LinuxVm + FullStandalone should succeed when qemu-img is available");
    let id = uuid::Uuid::parse_str(&response.into_inner().instance_id)
        .expect("clone response must contain a valid UUID");
    assert!(!id.is_nil());

    server.abort();
}

/// LinuxVm + SharedBase — должен вернуть `FAILED_PRECONDITION`
/// (`SharedBaseNotSupportedForLinuxVm`), потому что у LinuxVm нет
/// shared `base_image`, к которому можно сделать thin-клон.
#[tokio::test]
async fn clone_instance_linux_vm_shared_base_rejected_as_failed_precondition() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let status = client
        .clone_instance(CloneInstanceRequest {
            source_instance_id: create_response.instance_id,
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::SharedBase as i32,
        })
        .await
        .expect_err("LinuxVm + SharedBase must be rejected");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    server.abort();
}
