//! Бинарник `andler` — тонкий gRPC-клиент к `andlerd`. Никакой
//! бизнес-логики здесь: каждая подкоманда формирует один gRPC-запрос через
//! `andler-rpc`/`tonic` и печатает ответ. См. README.md этого крейта.

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidProfile as ProtoAndroidProfile, AndroidVersion as ProtoAndroidVersion,
    CreateAndroidInstanceRequest, InstanceIdRequest, InstanceStateKind, RootMode as ProtoRootMode,
    StopInstanceRequest,
};
use clap::{Parser, Subcommand, ValueEnum};

const DEFAULT_DAEMON_ADDR: &str = "http://127.0.0.1:50051";

#[derive(Parser)]
#[command(name = "andler", about = "Тонкий CLI-клиент к andlerd")]
struct Cli {
    /// Адрес andlerd. По умолчанию берётся ANDLERD_ADDR, иначе
    /// http://127.0.0.1:50051 (см. andler-daemon/src/main.rs).
    #[arg(long, global = true)]
    daemon_addr: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Резолвит AndroidProfile в инстанс и регистрирует его в andlerd.
    CreateAndroid {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value_t = CliAndroidVersion::Android13)]
        android_version: CliAndroidVersion,
        #[arg(long)]
        gapps: bool,
        #[arg(long)]
        microg: bool,
        #[arg(long)]
        libndk: bool,
        #[arg(long, value_enum, default_value_t = CliRootMode::None)]
        root: CliRootMode,
        #[arg(long)]
        base_image_path: String,
        #[arg(long)]
        instances_root: String,
        /// Размер overlay-диска в GiB (не байтах — удобнее для CLI).
        #[arg(long, default_value_t = 20)]
        overlay_size_gib: u64,
        #[arg(long)]
        ovmf_vars_template: String,
    },
    /// Запускает ранее созданный инстанс.
    Start { instance_id: String },
    /// Останавливает инстанс.
    Stop {
        instance_id: String,
        /// Без этого флага останавливает не дожидаясь graceful shutdown —
        /// см. HypervisorBackend::stop и текущие ограничения andler-qemu
        /// (нет полноценного ACPI-сигнала, см. README andler-qemu).
        #[arg(long)]
        graceful: bool,
    },
    /// Приостанавливает работающий инстанс.
    Pause { instance_id: String },
    /// Возобновляет приостановленный инстанс.
    Resume { instance_id: String },
    /// Печатает текущий статус инстанса.
    Status { instance_id: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum CliAndroidVersion {
    #[value(name = "11")]
    Android11,
    #[value(name = "13")]
    Android13,
}

impl From<CliAndroidVersion> for ProtoAndroidVersion {
    fn from(value: CliAndroidVersion) -> Self {
        match value {
            CliAndroidVersion::Android11 => ProtoAndroidVersion::Android11,
            CliAndroidVersion::Android13 => ProtoAndroidVersion::Android13,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CliRootMode {
    None,
    Magisk,
    Kernelsu,
}

impl From<CliRootMode> for ProtoRootMode {
    fn from(value: CliRootMode) -> Self {
        match value {
            CliRootMode::None => ProtoRootMode::None,
            CliRootMode::Magisk => ProtoRootMode::Magisk,
            CliRootMode::Kernelsu => ProtoRootMode::KernelSu,
        }
    }
}

fn state_kind_name(kind: InstanceStateKind) -> &'static str {
    match kind {
        InstanceStateKind::InstanceStateUnspecified => "UNSPECIFIED",
        InstanceStateKind::Created => "Created",
        InstanceStateKind::Starting => "Starting",
        InstanceStateKind::Running => "Running",
        InstanceStateKind::Paused => "Paused",
        InstanceStateKind::Stopping => "Stopping",
        InstanceStateKind::Stopped => "Stopped",
        InstanceStateKind::Error => "Error",
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let addr = cli
        .daemon_addr
        .or_else(|| std::env::var("ANDLERD_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_DAEMON_ADDR.to_string());

    let mut client = AndlerServiceClient::connect(addr).await?;

    match cli.command {
        Command::CreateAndroid {
            name,
            android_version,
            gapps,
            microg,
            libndk,
            root,
            base_image_path,
            instances_root,
            overlay_size_gib,
            ovmf_vars_template,
        } => {
            let mut profile = ProtoAndroidProfile {
                gapps,
                microg,
                libndk,
                ..Default::default()
            };
            profile.set_android_version(android_version.into());
            profile.set_root(root.into());

            let response = client
                .create_android_instance(CreateAndroidInstanceRequest {
                    name,
                    profile: Some(profile),
                    base_image_path,
                    instances_root,
                    overlay_size_bytes: overlay_size_gib * 1024 * 1024 * 1024,
                    ovmf_vars_template,
                })
                .await?;
            println!("{}", response.into_inner().instance_id);
        }
        Command::Start { instance_id } => {
            client
                .start_instance(InstanceIdRequest { instance_id })
                .await?;
            println!("started");
        }
        Command::Stop {
            instance_id,
            graceful,
        } => {
            client
                .stop_instance(StopInstanceRequest {
                    instance_id,
                    graceful,
                })
                .await?;
            println!("stopped");
        }
        Command::Pause { instance_id } => {
            client
                .pause_instance(InstanceIdRequest { instance_id })
                .await?;
            println!("paused");
        }
        Command::Resume { instance_id } => {
            client
                .resume_instance(InstanceIdRequest { instance_id })
                .await?;
            println!("resumed");
        }
        Command::Status { instance_id } => {
            let response = client
                .get_instance_status(InstanceIdRequest { instance_id })
                .await?
                .into_inner();
            let state = response.state();
            println!("state: {}", state_kind_name(state));
            if !response.detail.is_empty() {
                println!("detail: {}", response.detail);
            }
            if !response.error_message.is_empty() {
                println!("error: {}", response.error_message);
            }
        }
    }

    Ok(())
}
