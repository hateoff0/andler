use std::str::FromStr;
use std::sync::Arc;

use andler_core::InstanceId;
use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::andler_service_server::AndlerServiceServer;
use andler_rpc::proto::{
    AttachDiskRequest, AttachNetworkRequest, AudioConfig, CloneInstanceRequest, CloneMode,
    CpuConfig, CreateInstanceRequest, DetachDiskRequest, DetachNetworkRequest, DiskConfig,
    DisplayConfig, Empty, EventStreamRequest, ExecCommandRequest, ExportInstanceDiskRequest,
    ExportInstanceOciRequest, FirmwareConfig, GetInstanceConfigResponse, GpuConfig,
    GuestProvisionRequest, InputConfig, InstallGuestAgentRequest, InstanceIdRequest,
    InstanceStateKind, MemoryConfig, NetworkConfig, OpCancelRequest, ProvisionMkdirP,
    RemoveGuestAgentRequest, RemoveInstanceRequest, Resolution, RestoreSnapshotRequest,
    SetInstanceConfigRequest,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::daemon::Daemon;
use crate::firmware::OvmfPaths;
use crate::service::DaemonService;

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
    let service = DaemonService::new(
        daemon,
        test_ovmf,
        crate::log_ring::LogRing::new(),
        std::sync::Arc::new(crate::metrics::DaemonMetrics::default()),
    );

    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(AndlerServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("serve_with_incoming must not fail for a healthy listener");
    });

    let client = AndlerServiceClient::connect(format!("http://{addr}"))
        .await
        .expect("client must be able to connect to the just-spawned server");

    (client, server)
}

#[tokio::test]
async fn unknown_instance_round_trips_as_not_found_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;
    let unknown_id = "a".repeat(64);

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
        port_forwards: vec![],
    };

    let firmware = FirmwareConfig {
        enable_uefi: true,
        ovmf_code_path: "/usr/share/edk2/x64/OVMF_CODE.4m.fd".to_string(),
        ovmf_vars_path: String::new(),
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
            mem_lock: false,
            hugepages: false,
        }),
        disk: Some(disk),
        display: Some(display),
        gpu: Some(gpu),
        network: Some(network),
        firmware: Some(firmware),
        audio: Some(audio),
        input: Some(input),
        cdrom_bus: andler_rpc::proto::CdromBus::Ide as i32,
        autostart: false,
    }
}

#[tokio::test]
async fn create_instance_round_trips_over_real_grpc_and_status_reports_created() {
    let (mut client, server) = spawn_server_and_connect().await;

    let response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("a fully-populated CreateInstanceRequest must be accepted")
        .into_inner();

    let id = InstanceId::from_str(&response.instance_id)
        .expect("CreateInstanceResponse.instance_id must be a valid 64-hex ID");

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
async fn create_instance_honors_explicit_ovmf_vars_template() {
    let (mut client, server) = spawn_server_and_connect().await;

    let custom_template = std::env::temp_dir().join(format!(
        "andler-test-custom-ovmf-vars-{}.fd",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&custom_template, b"custom-vars-marker").expect("write test fixture file");

    let mut request = sample_create_instance_request();
    request.firmware.as_mut().unwrap().ovmf_vars_path =
        custom_template.to_string_lossy().into_owned();

    let response = client
        .create_instance(request)
        .await
        .expect("creation with an explicit, existing ovmf_vars_path must succeed")
        .into_inner();

    let config: GetInstanceConfigResponse = client
        .get_instance_config(InstanceIdRequest {
            instance_id: response.instance_id.clone(),
        })
        .await
        .expect("freshly created instance must be found")
        .into_inner();

    let provisioned_vars_path = config
        .firmware
        .expect("firmware must be set")
        .ovmf_vars_path;
    let provisioned_content =
        std::fs::read_to_string(&provisioned_vars_path).expect("provisioned VARS.fd must exist");
    assert_eq!(
        provisioned_content, "custom-vars-marker",
        "provisioned VARS.fd must be a copy of the client's explicit \
         ovmf_vars_path, not the daemon's auto-detected template"
    );

    let _ = std::fs::remove_file(&custom_template);
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
        port_forwards: vec![],
    });

    let response = client
        .create_instance(request)
        .await
        .expect("a request with NetworkMode::Bridge must be accepted")
        .into_inner();
    InstanceId::from_str(&response.instance_id)
        .expect("CreateInstanceResponse.instance_id must be a valid 64-hex ID");

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
            instance_id: "a".repeat(64),
            purge: false,
        })
        .await
        .expect_err("removing an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn get_instance_config_over_real_grpc_returns_what_was_created() {
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
    match gpu
        .render_backend
        .expect("render_backend must be Some")
        .kind
    {
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
            instance_id: "a".repeat(64),
        })
        .await
        .expect_err("get_instance_config on an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

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
            instance_id: "a".repeat(64),
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
            source_instance_id: "a".repeat(64),
            new_name: "clone".to_string(),
            instances_root: "/tmp/instances".to_string(),
            mode: CloneMode::Linked as i32,
        })
        .await
        .expect_err("cloning an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

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
    let id = InstanceId::from_str(&response.into_inner().instance_id)
        .expect("clone response must contain a valid 64-hex ID");
    assert_eq!(id.to_string().len(), 64);

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
            source_instance_id: "a".repeat(64),
            dest_path: "/tmp/export.qcow2".to_string(),
        })
        .await
        .expect_err("exporting an unregistered instance_id must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn export_instance_oci_on_unknown_source_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .export_instance_oci(ExportInstanceOciRequest {
            source_instance_id: "a".repeat(64),
            dest_path: "/tmp/export-oci".to_string(),
            disk_format: andler_rpc::proto::DiskFormat::Raw as i32,
            disk_path: None,
        })
        .await
        .expect_err("exporting an unregistered instance_id to OCI must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

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

#[tokio::test]
async fn export_instance_oci_linux_vm_with_qemu_img() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    // A small standalone qcow2 stands in for the instance's disk so the
    // qcow2->raw conversion stays fast instead of materializing the 40 GiB
    // default disk. The op's qcow2-format check still runs against it.
    let small_disk =
        std::env::temp_dir().join(format!("andler_oci_test_{}.qcow2", std::process::id()));
    let _ = std::fs::remove_file(&small_disk);
    {
        let status = std::process::Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                small_disk.to_string_lossy().as_ref(),
                "64M",
            ])
            .status()
            .expect("qemu-img must be available");
        assert!(status.success(), "qemu-img create must succeed");
    }

    let dest_path = std::env::temp_dir().join(format!("andler_oci_export_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_path);
    let response = client
        .export_instance_oci(ExportInstanceOciRequest {
            source_instance_id: create_response.instance_id,
            dest_path: dest_path.to_string_lossy().to_string(),
            disk_format: andler_rpc::proto::DiskFormat::Raw as i32,
            disk_path: Some(small_disk.to_string_lossy().to_string()),
        })
        .await
        .expect("exporting a LinuxVm to OCI should succeed when qemu-img is available");
    assert!(!response.into_inner().dest_path.is_empty());

    // The daemon wrote a conformant layout: the marker, index.json, config.json
    // and a blob named by its own sha256 digest.
    assert!(
        dest_path.join("oci-layout").exists(),
        "oci-layout marker must exist"
    );
    assert!(
        dest_path.join("index.json").exists(),
        "index.json must exist"
    );
    assert!(
        dest_path.join("config.json").exists(),
        "config.json must exist"
    );
    assert!(
        dest_path.join("blobs").join("sha256").exists(),
        "blobs/sha256 must exist"
    );

    let _ = std::fs::remove_dir_all(&dest_path);
    let _ = std::fs::remove_file(&small_disk);
    server.abort();
}

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
    let id = InstanceId::from_str(&response.into_inner().instance_id)
        .expect("clone response must contain a valid 64-hex ID");
    assert_eq!(id.to_string().len(), 64);

    server.abort();
}

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

#[tokio::test]
async fn create_android_instance_auto_resolves_base_image_when_omitted() {
    use andler_rpc::proto::{
        AndroidProfile as ProtoAndroidProfile, AndroidVersion as ProtoAndroidVersion,
        CreateAndroidInstanceRequest,
    };

    let (mut client, server) = spawn_server_and_connect().await;

    let andler_home = std::env::temp_dir().join(format!(
        "andler-grpc-test-android-home-{}",
        uuid::Uuid::new_v4()
    ));
    let base_images_dir = andler_home.join("cache").join("base-images");
    std::fs::create_dir_all(&base_images_dir).expect("create fake base-images dir");
    let fake_qcow2 = base_images_dir.join("linux-waydroid-android13-vanilla-test.qcow2");
    andler_disk::qcow2::create(&fake_qcow2, 1024 * 1024 * 1024)
        .await
        .expect("create fake base qcow2");
    std::fs::write(
        base_images_dir.join("linux-waydroid-android13-vanilla-test.manifest.json"),
        r#"{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2026-01-01T00:00:00Z"}"#,
    )
    .expect("write fake manifest");

    // A fresher image in the per-version-variant subdirectory must win over the
    // flat-root legacy layout (discovery scans both).
    let subdir = base_images_dir.join("android13-vanilla");
    std::fs::create_dir_all(&subdir).expect("create fake subdir");
    let subdir_qcow2 = subdir.join("linux-waydroid-android13-vanilla-new.qcow2");
    andler_disk::qcow2::create(&subdir_qcow2, 1024 * 1024 * 1024)
        .await
        .expect("create fake subdir base qcow2");
    std::fs::write(
        subdir.join("linux-waydroid-android13-vanilla-new.manifest.json"),
        r#"{"schema_version":1,"android_major":"13","android_variant":"VANILLA","built_at":"2026-06-01T00:00:00Z"}"#,
    )
    .expect("write fake subdir manifest");

    let instances_root = std::env::temp_dir().join(format!(
        "andler-grpc-test-android-instances-{}",
        uuid::Uuid::new_v4()
    ));
    let ovmf_vars_template = std::env::temp_dir().join(format!(
        "andler-grpc-test-android-ovmf-vars-{}.fd",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&ovmf_vars_template, b"fake-ovmf-vars").expect("write fake ovmf vars");

    // ANDLER_HOME is a process-wide env var; both scenarios below run sequentially
    // within this one test function so there's no race with a concurrent test.
    std::env::set_var(andler_core::paths::ANDLER_HOME_ENV, &andler_home);

    let mut profile = ProtoAndroidProfile::default();
    profile.set_android_version(ProtoAndroidVersion::Android13);

    let response = client
        .create_android_instance(CreateAndroidInstanceRequest {
            name: "auto-resolved-android".to_string(),
            profile: Some(profile),
            base_image_path: String::new(),
            instances_root: instances_root.to_string_lossy().into_owned(),
            overlay_size_bytes: 1024 * 1024 * 1024,
            ovmf_vars_template: ovmf_vars_template.to_string_lossy().into_owned(),
            linked_overlay: true,
        })
        .await
        .expect("a matching base image in base_images_dir() must be auto-resolved")
        .into_inner();

    let config: GetInstanceConfigResponse = client
        .get_instance_config(InstanceIdRequest {
            instance_id: response.instance_id,
        })
        .await
        .expect("freshly created instance must be found")
        .into_inner();
    assert_eq!(
        config.disk.expect("disk must be set").base_image,
        subdir_qcow2.to_string_lossy().into_owned(),
        "auto-resolved base image must be the freshest match, including subdirectory layout"
    );

    let mut unmatched_profile = ProtoAndroidProfile::default();
    unmatched_profile.set_android_version(ProtoAndroidVersion::Android11);

    let status = client
        .create_android_instance(CreateAndroidInstanceRequest {
            name: "no-matching-base-image".to_string(),
            profile: Some(unmatched_profile),
            base_image_path: String::new(),
            instances_root: instances_root.to_string_lossy().into_owned(),
            overlay_size_bytes: 1024 * 1024 * 1024,
            ovmf_vars_template: ovmf_vars_template.to_string_lossy().into_owned(),
            linked_overlay: false,
        })
        .await
        .expect_err("no Android 11 manifest exists in base_images_dir(), so this must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    std::env::remove_var(andler_core::paths::ANDLER_HOME_ENV);
    let _ = std::fs::remove_dir_all(&andler_home);
    let _ = std::fs::remove_dir_all(&instances_root);
    let _ = std::fs::remove_file(&ovmf_vars_template);
    server.abort();
}

#[tokio::test]
async fn create_android_instance_linked_overlay_false_produces_standalone_disk() {
    use andler_rpc::proto::{
        AndroidProfile as ProtoAndroidProfile, AndroidVersion as ProtoAndroidVersion,
        CreateAndroidInstanceRequest,
    };

    let (mut client, server) = spawn_server_and_connect().await;

    let base_image = std::env::temp_dir().join(format!(
        "andler-grpc-test-standalone-base-{}.qcow2",
        uuid::Uuid::new_v4()
    ));
    andler_disk::qcow2::create(&base_image, 512 * 1024 * 1024)
        .await
        .expect("create fake base qcow2");

    let instances_root = std::env::temp_dir().join(format!(
        "andler-grpc-test-standalone-instances-{}",
        uuid::Uuid::new_v4()
    ));
    let ovmf_vars_template = std::env::temp_dir().join(format!(
        "andler-grpc-test-standalone-ovmf-vars-{}.fd",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&ovmf_vars_template, b"fake-ovmf-vars").expect("write fake ovmf vars");

    let mut profile = ProtoAndroidProfile::default();
    profile.set_android_version(ProtoAndroidVersion::Android13);

    let response = client
        .create_android_instance(CreateAndroidInstanceRequest {
            name: "standalone-android".to_string(),
            profile: Some(profile),
            base_image_path: base_image.to_string_lossy().into_owned(),
            instances_root: instances_root.to_string_lossy().into_owned(),
            overlay_size_bytes: 512 * 1024 * 1024,
            ovmf_vars_template: ovmf_vars_template.to_string_lossy().into_owned(),
            linked_overlay: false,
        })
        .await
        .expect("standalone (full-copy) creation must succeed")
        .into_inner();

    let config: GetInstanceConfigResponse = client
        .get_instance_config(InstanceIdRequest {
            instance_id: response.instance_id.clone(),
        })
        .await
        .expect("freshly created instance must be found")
        .into_inner();
    let disk = config.disk.expect("disk must be set");
    assert!(
        disk.base_image.is_empty(),
        "linked_overlay: false must produce a standalone disk with no backing file"
    );

    let instance_dir = instances_root.join(&response.instance_id);
    assert!(
        instance_dir.join("disk.qcow2").exists(),
        "standalone disk file must be created on disk"
    );

    let _ = std::fs::remove_file(&base_image);
    let _ = std::fs::remove_dir_all(&instances_root);
    let _ = std::fs::remove_file(&ovmf_vars_template);
    server.abort();
}

#[tokio::test]
async fn list_guest_packages_instance_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .list_guest_packages(InstanceIdRequest {
            instance_id: "b".repeat(64),
        })
        .await
        .expect_err("nonexistent instance must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn provision_on_unknown_instance_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .guest_provision(GuestProvisionRequest {
            instance_id: "c".repeat(64),
            ops: vec![provision_mkdir_op()],
        })
        .await
        .expect_err("nonexistent instance must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn provision_with_empty_ops_round_trips_as_invalid_argument() {
    let (mut client, server) = spawn_server_and_connect().await;

    let status = client
        .guest_provision(GuestProvisionRequest {
            instance_id: "d".repeat(64),
            ops: vec![],
        })
        .await
        .expect_err("empty ops must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

fn provision_mkdir_op() -> andler_rpc::proto::ProvisionOp {
    andler_rpc::proto::ProvisionOp {
        op: Some(andler_rpc::proto::provision_op::Op::MkdirP(
            ProvisionMkdirP {
                path: "/opt/x".to_string(),
            },
        )),
    }
}

#[tokio::test]
async fn stream_daemon_logs_snapshot_round_trips_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    let mut stream = client
        .stream_daemon_logs(andler_rpc::proto::DaemonLogsRequest {
            follow: false,
            since_ms: None,
        })
        .await
        .unwrap()
        .into_inner();

    // The test server's ring is empty, but the RPC itself must work and
    // terminate (snapshot mode closes the stream).
    let mut lines = 0;
    while let Some(line) = stream.message().await.unwrap() {
        assert!(!line.line.is_empty());
        lines += 1;
    }
    assert_eq!(lines, 0, "fresh ring has no lines");

    server.abort();
}

#[tokio::test]
async fn get_daemon_metrics_round_trips_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    let snap = client
        .get_daemon_metrics(Empty {})
        .await
        .unwrap()
        .into_inner();
    // The test server's metrics layer is not installed (the snapshot is
    // taken in main.rs), so latency may be empty — but the counters must
    // be present and sane.
    assert_eq!(snap.instance_count, 0);
    assert_eq!(snap.running_count, 0);
    assert_eq!(snap.active_ops, 0);
    assert_eq!(snap.qmp_reconnects, 0);
    assert_eq!(snap.running_ram_bytes, 0);

    server.abort();
}

#[tokio::test]
async fn set_instance_config_display_resolution_updates_config_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    client
        .set_instance_config(SetInstanceConfigRequest {
            instance_ref: create_response.instance_id.clone(),
            key: "display.resolution".to_string(),
            value: "1920x1200".to_string(),
        })
        .await
        .expect("set display.resolution on a stopped instance must succeed");

    let config = client
        .get_instance_config(InstanceIdRequest {
            instance_id: create_response.instance_id,
        })
        .await
        .expect("get_instance_config must succeed")
        .into_inner();
    let display = config.display.expect("display must be Some");
    assert_eq!(
        display.resolution.as_ref().map(|r| (r.width, r.height)),
        Some((1920, 1200))
    );

    server.abort();
}

#[tokio::test]
async fn set_instance_config_display_resolution_rejects_malformed_value() {
    let (mut client, server) = spawn_server_and_connect().await;

    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let status = client
        .set_instance_config(SetInstanceConfigRequest {
            instance_ref: create_response.instance_id,
            key: "display.resolution".to_string(),
            value: "banana".to_string(),
        })
        .await
        .expect_err("malformed resolution must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn attach_disk_on_created_instance_round_trips_as_failed_precondition() {
    let (mut client, server) = spawn_server_and_connect().await;
    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let status = client
        .attach_disk(AttachDiskRequest {
            instance_id: create_response.instance_id.clone(),
            path: String::new(),
            size_bytes: 8 * 1024 * 1024 * 1024,
        })
        .await
        .expect_err("hotplug requires the instance to be running or paused");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        status.message().contains("start it first"),
        "state gate message must tell the user what to do: {}",
        status.message()
    );

    let status = client
        .detach_disk(DetachDiskRequest {
            instance_id: create_response.instance_id.clone(),
            path: "/tmp/x.qcow2".to_string(),
        })
        .await
        .expect_err("detach also requires running or paused");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    server.abort();
}

#[tokio::test]
async fn get_config_status_reports_file_vs_memory_diff_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    // Unique disk dir so the manual instance.toml write below cannot race
    // with other round-trip tests using the shared /tmp/disk.qcow2.
    let dir = std::env::temp_dir().join(format!("andler-rt-cfg-{}", InstanceId::new()));
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let mut request = sample_create_instance_request();
    request.disk.as_mut().unwrap().path = dir.join("disk.qcow2").to_string_lossy().into_owned();
    let create_response = client
        .create_instance(request)
        .await
        .expect("create_instance must succeed")
        .into_inner();
    let id = create_response.instance_id;

    let status = client
        .get_config_status(InstanceIdRequest {
            instance_id: id.clone(),
        })
        .await
        .expect("get_config_status must succeed")
        .into_inner();
    assert_eq!(status.instance_id, id);
    assert!(
        status.diffs.is_empty(),
        "fresh instance: file and memory in sync"
    );
    assert!(status.file_error.is_none());

    // A manual edit of instance.toml (the documented way to change a stopped
    // instance) is picked up: on an idle instance the file is the source of
    // truth, so the status is in sync and the loaded config reflects it.
    let original: andler_core::InstanceConfig = client
        .get_instance_config(InstanceIdRequest {
            instance_id: id.clone(),
        })
        .await
        .expect("get_instance_config must succeed")
        .into_inner()
        .try_into()
        .expect("round-trip conversion must succeed");
    let mut edited = original.clone();
    edited.name = "hand-edited-name".to_string();
    let instance_dir = edited.disk.path.parent().expect("disk must have a parent");
    tokio::fs::write(
        instance_dir.join("instance.toml"),
        toml::to_string_pretty(&edited).expect("serialize"),
    )
    .await
    .unwrap();

    let status = client
        .get_config_status(InstanceIdRequest {
            instance_id: id.clone(),
        })
        .await
        .expect("get_config_status must succeed")
        .into_inner();
    assert!(
        status.diffs.is_empty(),
        "idle instance: file edit is applied, not pending: {:?}",
        status.diffs
    );

    let config = client
        .get_instance_config(InstanceIdRequest { instance_id: id })
        .await
        .expect("get_instance_config must succeed")
        .into_inner();
    assert_eq!(config.name, "hand-edited-name");

    let _ = tokio::fs::remove_dir_all(&dir).await;
    server.abort();
}

#[tokio::test]
async fn attach_network_on_created_instance_round_trips_as_failed_precondition() {
    let (mut client, server) = spawn_server_and_connect().await;
    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let mut network = NetworkConfig {
        mode: None,
        device_model: "virtio-net-pci".to_string(),
        ..Default::default()
    };
    network.set_nat_backend(andler_rpc::proto::NatBackend::Slirp);
    network.mode = Some(andler_rpc::proto::NetworkMode {
        kind: Some(andler_rpc::proto::network_mode::Kind::Nat(
            andler_rpc::proto::network_mode::Nat {},
        )),
    });

    let status = client
        .attach_network(AttachNetworkRequest {
            instance_id: create_response.instance_id,
            network: Some(network),
        })
        .await
        .expect_err("hotplug requires the instance to be running or paused");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    server.abort();
}

#[tokio::test]
async fn hotplug_on_unknown_instance_round_trips_as_not_found() {
    let (mut client, server) = spawn_server_and_connect().await;
    let unknown_id = "b".repeat(64);

    let status = client
        .attach_disk(AttachDiskRequest {
            instance_id: unknown_id.clone(),
            path: String::new(),
            size_bytes: 1024,
        })
        .await
        .expect_err("unknown instance must produce NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .detach_network(DetachNetworkRequest {
            instance_id: unknown_id.clone(),
            index: 0,
        })
        .await
        .expect_err("unknown instance must produce NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .attach_network(AttachNetworkRequest {
            instance_id: unknown_id.clone(),
            network: None,
        })
        .await
        .expect_err("missing network field must be rejected as invalid_argument");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let mut network = NetworkConfig {
        mode: None,
        device_model: "virtio-net-pci".to_string(),
        ..Default::default()
    };
    network.set_nat_backend(andler_rpc::proto::NatBackend::Slirp);
    network.mode = Some(andler_rpc::proto::NetworkMode {
        kind: Some(andler_rpc::proto::network_mode::Kind::Isolated(
            andler_rpc::proto::network_mode::Isolated {},
        )),
    });
    let status = client
        .attach_network(AttachNetworkRequest {
            instance_id: unknown_id,
            network: Some(network),
        })
        .await
        .expect_err("unknown instance with a valid network must produce NOT_FOUND");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn config_round_trip_preserves_extra_devices_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;
    let create_response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create_instance must succeed")
        .into_inner();

    let mut config = client
        .get_instance_config(InstanceIdRequest {
            instance_id: create_response.instance_id.clone(),
        })
        .await
        .expect("get_instance_config must succeed")
        .into_inner();
    assert!(
        config.extra_disks.is_empty() && config.extra_networks.is_empty(),
        "a fresh instance must start with no hotplugged devices"
    );

    config.extra_disks.push(andler_rpc::proto::DiskConfig {
        path: "/data/extra.qcow2".to_string(),
        size_bytes: 1024,
        format: 1, // QCOW2
        base_image: String::new(),
        thin_provisioning: true,
        trim_on_shutdown: false,
        compact_on_shutdown: false,
        snapshot_timeout_secs: None,
        ..Default::default()
    });

    // UpdateInstanceConfigRequest round-trips through the daemon's update path.
    let update = andler_rpc::convert::instance_config_to_update_request(
        config
            .clone()
            .try_into()
            .expect("response must convert back to a domain config"),
        create_response.instance_id.clone(),
    );
    client
        .update_instance_config(update)
        .await
        .expect("update must succeed");

    let config_after = client
        .get_instance_config(InstanceIdRequest {
            instance_id: create_response.instance_id,
        })
        .await
        .expect("get_instance_config must succeed")
        .into_inner();
    assert_eq!(config_after.extra_disks.len(), 1);

    server.abort();
}

#[tokio::test]
async fn snapshot_restore_branch_flag_round_trips_and_list_is_empty_for_fresh_instance() {
    let (mut client, server) = spawn_server_and_connect().await;

    let response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("a fully-populated CreateInstanceRequest must be accepted")
        .into_inner();

    // RestoreSnapshotRequest.branch reaches the daemon through the wire.
    // The instance has no store and no internal snapshot, so restore must
    // fail — but with a daemon-side error, not a transport/parse failure.
    let status = client
        .restore_snapshot(RestoreSnapshotRequest {
            instance_id: response.instance_id.clone(),
            tag: "roundtrip-branch-test".to_string(),
            timeout_secs: None,
            branch: true,
            idempotency_token: None,
        })
        .await
        .expect_err("restore of a nonexistent snapshot must fail server-side");
    assert_ne!(status.code(), tonic::Code::Ok);
    assert_ne!(status.code(), tonic::Code::Unimplemented);

    // SnapshotEntry.branch serializes on the wire; fresh instance has none.
    let listed = client
        .list_snapshots(InstanceIdRequest {
            instance_id: response.instance_id,
        })
        .await
        .expect("list_snapshots must succeed on a fresh instance")
        .into_inner();
    assert!(listed.snapshots.is_empty());

    server.abort();
}

#[tokio::test]
async fn op_list_and_cancel_round_trip_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    // Fresh daemon: no active operations.
    let listed = client
        .list_operations(Empty {})
        .await
        .expect("list_operations must succeed")
        .into_inner();
    assert!(listed.operations.is_empty());

    // Cancelling an unknown operation is a not-found error, not a panic.
    let status = client
        .cancel_operation(OpCancelRequest {
            op_id: "op-unknown".to_string(),
        })
        .await
        .expect_err("cancelling an unknown operation must fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}

#[tokio::test]
async fn exec_command_round_trips_and_requires_running_instance() {
    let (mut client, server) = spawn_server_and_connect().await;

    let response = client
        .create_instance(sample_create_instance_request())
        .await
        .expect("a fully-populated CreateInstanceRequest must be accepted")
        .into_inner();

    // Exec on a fresh (not started) instance fails with a state error —
    // proving the ExecCommandRequest wire path reaches the daemon.
    let status = client
        .exec_command(ExecCommandRequest {
            instance_id: response.instance_id.clone(),
            argv: vec!["echo".to_string(), "hi".to_string()],
            timeout_secs: None,
        })
        .await
        .expect_err("exec on a stopped instance must fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);

    // Empty argv is rejected as invalid.
    let status = client
        .exec_command(ExecCommandRequest {
            instance_id: response.instance_id,
            argv: vec![],
            timeout_secs: None,
        })
        .await
        .expect_err("empty argv must fail");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.abort();
}

#[tokio::test]
async fn get_version_round_trips_and_matches_build() {
    let (mut client, server) = spawn_server_and_connect().await;

    let version = client
        .get_version(Empty {})
        .await
        .expect("get_version must succeed over real gRPC")
        .into_inner()
        .version;
    assert_eq!(version, env!("CARGO_PKG_VERSION"));

    server.abort();
}

#[tokio::test]
async fn stream_events_delivers_lifecycle_events_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;

    let mut events = client
        .stream_events(EventStreamRequest {
            instance_id: String::new(),
        })
        .await
        .expect("subscribe before creating")
        .into_inner();

    client
        .create_instance(sample_create_instance_request())
        .await
        .expect("create must succeed");

    let message = tokio::time::timeout(std::time::Duration::from_secs(3), events.message())
        .await
        .expect("a lifecycle event must arrive")
        .expect("stream must stay open")
        .expect("message must be ok");
    assert_eq!(message.kind, "Log");
    assert!(
        message.detail.contains("instance created"),
        "detail: {}",
        message.detail
    );

    server.abort();
}

/// The `idempotency_token` field on the three long-running requests is
/// transmitted over real gRPC: each request with a token set round-trips
/// through the wire and reaches the handler (an unknown instance is
/// reported as NOT_FOUND, not a wire/parse error).
#[tokio::test]
async fn idempotency_token_field_round_trips_over_real_grpc() {
    let (mut client, server) = spawn_server_and_connect().await;
    let unknown_id = "a".repeat(64);
    let token = "client-retry-token-abc123";

    let status = client
        .install_guest_agent(InstallGuestAgentRequest {
            instance_id: unknown_id.clone(),
            package: "htop".to_string(),
            offline: false,
            idempotency_token: Some(token.to_string()),
        })
        .await
        .expect_err("an unknown instance must be NOT_FOUND, not a wire error");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .remove_guest_agent(RemoveGuestAgentRequest {
            instance_id: unknown_id.clone(),
            package: "htop".to_string(),
            offline: false,
            idempotency_token: Some(token.to_string()),
        })
        .await
        .expect_err("an unknown instance must be NOT_FOUND, not a wire error");
    assert_eq!(status.code(), tonic::Code::NotFound);

    let status = client
        .restore_snapshot(RestoreSnapshotRequest {
            instance_id: unknown_id.clone(),
            tag: "snap".to_string(),
            timeout_secs: Some(30),
            branch: false,
            idempotency_token: Some(token.to_string()),
        })
        .await
        .expect_err("an unknown instance must be NOT_FOUND, not a wire error");
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.abort();
}
