//! Бинарник `andler` — тонкий gRPC-клиент к `andlerd`. Никакой
//! бизнес-логики здесь: каждая подкоманда формирует один gRPC-запрос через
//! `andler-rpc`/`tonic` и печатает ответ. См. README.md этого крейта.

mod instance_file;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    instance_kind, network_mode, render_backend, AndroidProfile as ProtoAndroidProfile,
    AndroidVersion as ProtoAndroidVersion, AudioBackend, BackendKind, CloneInstanceRequest,
    CpuPriority, CreateAndroidInstanceRequest, CreateSnapshotRequest, DeleteSnapshotRequest,
    DiskFormat, DisplayEngine, Empty, ExportInstanceDiskRequest, GetInstanceConfigResponse,
    InstanceIdRequest, InstanceStateKind, LogStreamSource,
    RemoveInstanceRequest, RestoreSnapshotRequest, RootMode as ProtoRootMode, StopInstanceRequest,
};
use clap::{Parser, Subcommand, ValueEnum};
use instance_file::InstanceFile;
use std::path::PathBuf;

const DEFAULT_DAEMON_ADDR: &str = "http://127.0.0.1:50051";

fn default_instances_root() -> String {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("andler/instances")
        .to_string_lossy()
        .into_owned()
}

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
    /// Создаёт новый инстанс. Два режима:
    ///
    /// **TOML-режим** (`--file`): создаёт LinuxVm из TOML-файла конфигурации
    /// — см. `andler-cli/src/instance_file.rs` за полем `InstanceFile` и
    /// README этого крейта за примером файла.
    ///
    /// Пример: `andler create --file instance.toml`
    ///
    /// **CLI-режим** (`--android-version`): создаёт AndroidVm через
    /// CLI-флаги. Обязательны `--name`, `--base-image-path`,
    /// `--ovmf-vars-template`.
    ///
    /// Пример: `andler create --name my-android --android-version 13 --base-image-path /path/to/base.qcow2 --ovmf-vars-template /path/to/VARS.fd`
    Create {
        /// Путь к TOML-файлу (TOML-режим). Укажите либо `--file`,
        /// либо `--android-version` — оба одновременно нельзя.
        #[arg(long)]
        file: Option<PathBuf>,

        // --- Android-режим (CLI-флаги) ---

        /// Имя инстанса (обязательно в Android-режиме).
        #[arg(long)]
        name: Option<String>,

        /// Версия Android — дискриминатор режима: если указан,
        /// создаётся AndroidVm (CLI-режим).
        #[arg(long, value_enum)]
        android_version: Option<CliAndroidVersion>,

        #[arg(long)]
        gapps: bool,
        #[arg(long)]
        microg: bool,
        #[arg(long)]
        libndk: bool,

        #[arg(long, value_enum, default_value_t = CliRootMode::None)]
        root: CliRootMode,

        /// Путь к базовому образу Android (обязательно в Android-режиме).
        #[arg(long)]
        base_image_path: Option<String>,

        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,

        /// Размер overlay-диска в GiB (не байтах — удобнее для CLI).
        #[arg(long, default_value_t = 20)]
        overlay_size_gib: u64,

        /// Путь к шаблону OVMF_VARS (обязательно в Android-режиме).
        #[arg(long)]
        ovmf_vars_template: Option<String>,

        /// Каталог с бинарниками Magisk (обязательно при --root magisk).
        /// Должен содержать как минимум `magisk` и `magiskinit` — результат
        /// распаковки Magisk release ZIP.
        #[arg(long)]
        magisk_dir: Option<PathBuf>,
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
    /// Печатает список всех зарегистрированных инстансов
    /// (id/имя/состояние). См. `Daemon::list_instances` — состояние тут
    /// грубое (запись демона, не live backend-статус); для точного
    /// статуса конкретного инстанса используй `status`.
    List,
    /// Удаляет запись инстанса. Требует, чтобы инстанс был остановлен
    /// (`Created`/`Stopped`/`Error`) — см. `Daemon::remove_instance` за
    /// тем, почему запущенный инстанс нужно сначала явно `stop`нуть.
    /// По умолчанию не удаляет файлы инстанса с диска — добавь `--purge`,
    /// чтобы дополнительно удалить disk.path и firmware.ovmf_vars_path
    /// (никогда base_image/ovmf_code_path — общие файлы, см.
    /// `Daemon::remove_instance`).
    Remove {
        instance_id: String,
        /// Дополнительно удалить файлы инстанса с диска (диск, личная
        /// копия OVMF_VARS). См. описание команды выше.
        #[arg(long)]
        purge: bool,
    },
    /// Печатает полную конфигурацию инстанса (все 9 секций), не только
    /// сводку из `list`. См. `Daemon::get_instance_config`.
    Config { instance_id: String },
    /// Стримит stdout/stderr процесса гипервизора инстанса в реальном
    /// времени (live-tail) — см. `Daemon::stream_instance_logs`. Не
    /// показывает строки, написанные до подключения (нет истории, см.
    /// документацию там же), и завершается сразу же без ошибки, если у
    /// инстанса сейчас нет запущенного backend'а (ещё не стартовал, либо
    /// уже остановлен) — в этом случае печатает предупреждение в stderr
    /// и завершается с кодом 0, не как при невалидном запросе.
    Logs { instance_id: String },
    /// Стримит метрики ресурсов инстанса (CPU%, RAM, disk I/O, net I/O)
    /// в реальном времени — см. `Daemon::stream_resource_metrics`.
    /// Метрики обновляются каждую секунду из `/proc/<pid>/`. Завершается
    /// сразу без ошибки, если инстанс не найден или не имеет запущенного
    /// backend'а.
    Metrics { instance_id: String },
    /// Клонирует инстанс (AndroidVm или LinuxVm) в новый, независимый
    /// инстанс. `LinuxVm` поддерживается для режимов `linked`/
    /// `full-standalone`; `shared-base` только для `AndroidVm` (см.
    /// `Daemon::clone_instance`). Источник должен быть остановлен
    /// (`Created`/`Stopped`/`Error`), как и для `remove`.
    Clone {
        source_instance_id: String,
        /// Имя нового инстанса.
        #[arg(long)]
        name: String,
        /// Каталог, под которым создаётся `<instances_root>/<новый_id>/`
        /// для файлов клона — тот же смысл, что у `instances_root` в
        /// Android-режиме `create` (см. там).
        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,
        /// Режим клонирования диска — см. `CliCloneMode` за описанием
        /// каждого варианта.
        #[arg(long, value_enum)]
        mode: CliCloneMode,
    },
    /// Экспортирует диск инстанса (AndroidVm или LinuxVm) в
    /// самостоятельный файл по указанному пути — для переноса между
    /// хостами или бэкапа, не создаёт новый инстанс (в отличие от
    /// `clone --mode full-standalone`, который создаёт). См.
    /// `Daemon::export_instance_disk`.
    Export {
        source_instance_id: String,
        dest_path: String,
    },
    /// Управление снапшотами инстанса. Инстанс должен быть запущен
    /// (`Running`/`Paused`) для создания снапшота, или остановлен
    /// (`Stopped`/`Created`/`Error`) для восстановления/удаления.
    Snapshot {
        instance_id: String,
        #[command(subcommand)]
        action: SnapshotAction,
    },
}

/// Действия со снапшотами.
#[derive(Subcommand)]
enum SnapshotAction {
    /// Создать снапшот текущего состояния (требует Running/Paused).
    Create {
        /// Имя снапшота (уникальное в пределах инстанса).
        #[arg(long)]
        tag: String,
        /// Необязательное описание.
        #[arg(long)]
        description: Option<String>,
    },
    /// Восстановить инстанс из снапшота (требует остановленный инстанс).
    Restore {
        /// Имя снапшота для восстановления.
        #[arg(long)]
        tag: String,
    },
    /// Удалить снапшот (требует остановленный инстанс).
    Delete {
        /// Имя снапшота для удаления.
        #[arg(long)]
        tag: String,
    },
    /// Показать список снапшотов инстанса.
    List,
}

/// Соответствует `andler_core::CloneMode` (через `andler_rpc::proto::CloneMode`)
/// один-к-одному — см. документацию `CloneMode` за полным обоснованием
/// каждого варианта; здесь только краткое напоминание для `--help`.
#[derive(Clone, Copy, ValueEnum)]
enum CliCloneMode {
    /// Дёшево и быстро, но клон зависит от источника — `remove --purge`
    /// источника откажет, пока клон жив.
    Linked,
    /// Полностью самостоятельный файл, дороже по месту/времени — не
    /// зависит ни от источника, ни от общего базового образа.
    #[value(name = "full-standalone")]
    FullStandalone,
    /// Не зависит от источника физически, но остаётся тонким
    /// относительно общего базового образа профиля.
    #[value(name = "shared-base")]
    SharedBase,
}

impl From<CliCloneMode> for andler_rpc::proto::CloneMode {
    fn from(value: CliCloneMode) -> Self {
        match value {
            CliCloneMode::Linked => andler_rpc::proto::CloneMode::Linked,
            CliCloneMode::FullStandalone => andler_rpc::proto::CloneMode::FullStandalone,
            CliCloneMode::SharedBase => andler_rpc::proto::CloneMode::SharedBase,
        }
    }
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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliRootMode {
    None,
    Magisk,
}

impl From<CliRootMode> for ProtoRootMode {
    fn from(value: CliRootMode) -> Self {
        match value {
            CliRootMode::None => ProtoRootMode::None,
            CliRootMode::Magisk => ProtoRootMode::Magisk,
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

fn backend_kind_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Unspecified => "UNSPECIFIED",
        BackendKind::Qemu => "Qemu",
        BackendKind::Vmm => "Vmm",
    }
}

/// Форматирует байты в человекочитаемый вид (KB/MB/GB).
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}B")
    }
}

/// Форматирует байты/сек в человекочитаемый вид.
fn format_bytes_per_sec(bps: u64) -> String {
    format!("{}/s", format_bytes(bps))
}

/// Печатает `GetInstanceConfigResponse` человекочитаемо — не валидный
/// TOML 1:1 (не претендует на формат `InstanceFile`/`andler create
/// --file`), но достаточно структурированный, чтобы можно было вручную
/// перенести значения в новый файл конфигурации, если нужно создать
/// похожий инстанс.
fn print_instance_config(config: GetInstanceConfigResponse) {
    println!("instance_id: {}", config.instance_id);
    println!("name: {}", config.name);
    println!("backend: {}", backend_kind_name(config.backend()));

    match config.kind.and_then(|k| k.kind) {
        Some(instance_kind::Kind::LinuxVm(linux_vm)) => {
            println!("kind: LinuxVm");
            println!("  iso_path: {}", linux_vm.iso_path);
        }
        Some(instance_kind::Kind::AndroidVm(android_vm)) => {
            println!("kind: AndroidVm");
            if let Some(profile) = android_vm.android_profile {
                println!("  android_version: {:?}", profile.android_version());
                println!("  gapps: {}", profile.gapps);
                println!("  microg: {}", profile.microg);
                println!("  libndk: {}", profile.libndk);
                println!("  root: {:?}", profile.root());
            }
        }
        None => println!("kind: <missing>"),
    }

    if let Some(cpu) = config.cpu {
        println!("[cpu]");
        println!("  cores: {}", cpu.cores);
        println!("  sockets: {}", cpu.sockets);
        println!("  threads: {}", cpu.threads);
        println!("  affinity: {:?}", cpu.affinity);
        println!(
            "  priority: {}",
            match cpu.priority() {
                CpuPriority::Unspecified => "UNSPECIFIED",
                CpuPriority::Low => "Low",
                CpuPriority::Normal => "Normal",
                CpuPriority::High => "High",
            }
        );
    }

    if let Some(memory) = config.memory {
        println!("[memory]");
        println!("  size_bytes: {}", memory.size_bytes);
        println!("  ballooning: {}", memory.ballooning);
        println!("  zram: {}", memory.zram);
        println!("  ksm: {}", memory.ksm);
    }

    if let Some(disk) = config.disk {
        println!("[disk]");
        println!("  path: {}", disk.path);
        println!("  size_bytes: {}", disk.size_bytes);
        println!(
            "  format: {}",
            match disk.format() {
                DiskFormat::Unspecified => "UNSPECIFIED",
                DiskFormat::Qcow2 => "Qcow2",
                DiskFormat::Raw => "Raw",
                DiskFormat::Vdi => "Vdi",
            }
        );
        if !disk.base_image.is_empty() {
            println!("  base_image: {}", disk.base_image);
        }
        println!("  thin_provisioning: {}", disk.thin_provisioning);
        println!("  trim_on_shutdown: {}", disk.trim_on_shutdown);
    }

    if let Some(display) = config.display {
        println!("[display]");
        if let Some(resolution) = display.resolution {
            println!("  resolution: {}x{}", resolution.width, resolution.height);
        }
        println!("  dpi: {}", display.dpi);
        println!("  fps_limit: {}", display.fps_limit);
        println!(
            "  display_engine: {}",
            match display.display_engine() {
                DisplayEngine::Unspecified => "UNSPECIFIED",
                DisplayEngine::Sdl => "Sdl",
                DisplayEngine::Spice => "Spice",
                DisplayEngine::Dbus => "Dbus",
                DisplayEngine::DisplayNone => "None",
            }
        );
        println!("  fullscreen: {}", display.fullscreen);
    }

    if let Some(gpu) = config.gpu {
        println!("[gpu]");
        println!("  hostmem_bytes: {}", gpu.hostmem_bytes);
        println!("  blob: {}", gpu.blob);
        println!("  gl: {}", gpu.gl);
        match gpu.render_backend.and_then(|rb| rb.kind) {
            Some(render_backend::Kind::Venus(_)) => println!("  render_backend: Venus"),
            Some(render_backend::Kind::VirtioGpu(_)) => println!("  render_backend: VirtioGpu"),
            Some(render_backend::Kind::VirGl(_)) => println!("  render_backend: VirGl"),
            Some(render_backend::Kind::Cpu(_)) => println!("  render_backend: Cpu"),
            Some(render_backend::Kind::Passthrough(p)) => {
                println!("  render_backend: Passthrough({})", p.gpu_pci_id)
            }
            None => println!("  render_backend: <missing>"),
        }
    }

    if let Some(network) = config.network {
        println!("[network]");
        println!("  device_model: {}", network.device_model);
        match network.mode.and_then(|m| m.kind) {
            Some(network_mode::Kind::Nat(_)) => println!("  mode: Nat"),
            Some(network_mode::Kind::Bridge(b)) => {
                println!("  mode: Bridge({})", b.interface)
            }
            Some(network_mode::Kind::Isolated(_)) => println!("  mode: Isolated"),
            None => println!("  mode: <missing>"),
        }
    }

    if let Some(firmware) = config.firmware {
        println!("[firmware]");
        println!("  ovmf_code_path: {}", firmware.ovmf_code_path);
        println!("  ovmf_vars_path: {}", firmware.ovmf_vars_path);
    }

    if let Some(audio) = config.audio {
        println!("[audio]");
        println!(
            "  backend: {}",
            match audio.backend() {
                AudioBackend::Unspecified => "UNSPECIFIED",
                AudioBackend::Pipewire => "Pipewire",
                AudioBackend::Pulseaudio => "Pulseaudio",
                AudioBackend::AudioNone => "None",
            }
        );
    }

    if let Some(input) = config.input {
        println!("[input]");
        println!("  tablet_mode: {}", input.tablet_mode);
        println!("  hide_host_cursor: {}", input.hide_host_cursor);
        println!("  clipboard_enabled: {}", input.clipboard_enabled);
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
        Command::Create {
            file,
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
            magisk_dir,
        } => {
            let has_file = file.is_some();
            let has_android = android_version.is_some();

            if has_file == has_android {
                eprintln!(
                    "уточните режим: либо --file (LinuxVm из TOML), либо --android-version (AndroidVm из CLI-флагов)"
                );
                std::process::exit(2);
            }

            if has_file {
                let file = file.unwrap();
                let instance_file = InstanceFile::load(&file)?;
                let response = client
                    .create_instance(instance_file.into_request())
                    .await?;
                println!("{}", response.into_inner().instance_id);
            } else {
                let name = match name {
                    Some(n) => n,
                    None => {
                        eprintln!("--name обязателен в Android-режиме (--android-version)");
                        std::process::exit(2);
                    }
                };
                let base_image_path = match base_image_path {
                    Some(p) => p,
                    None => {
                        eprintln!("--base-image-path обязателен в Android-режиме");
                        std::process::exit(2);
                    }
                };
                let ovmf_vars_template = match ovmf_vars_template {
                    Some(t) => t,
                    None => {
                        eprintln!("--ovmf-vars-template обязателен в Android-режиме");
                        std::process::exit(2);
                    }
                };
                let android_version = android_version.unwrap();

                if root == CliRootMode::Magisk && magisk_dir.is_none() {
                    eprintln!("--magisk-dir обязателен при --root magisk");
                    std::process::exit(2);
                }

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
                        magisk_dir: magisk_dir
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    })
                    .await?;
                println!("{}", response.into_inner().instance_id);
            }
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
        Command::List => {
            let response = client.list_instances(Empty {}).await?.into_inner();
            if response.instances.is_empty() {
                println!("no instances");
            } else {
                for entry in response.instances {
                    println!(
                        "{}  {}  {}",
                        entry.instance_id,
                        state_kind_name(entry.state()),
                        entry.name
                    );
                }
            }
        }
        Command::Remove { instance_id, purge } => {
            client
                .remove_instance(RemoveInstanceRequest {
                    instance_id,
                    purge,
                })
                .await?;
            println!("removed");
        }
        Command::Config { instance_id } => {
            let response = client
                .get_instance_config(InstanceIdRequest { instance_id })
                .await?
                .into_inner();
            print_instance_config(response);
        }
        Command::Logs { instance_id } => {
            let mut stream = client
                .stream_instance_logs(InstanceIdRequest { instance_id })
                .await?
                .into_inner();

            // Первое сообщение решает, печатать ли предупреждение о
            // пустом потоке — нельзя просто проверить "пуст ли стрим" до
            // первого `.message()`, у tonic-клиента нет такого метода
            // отдельно от попытки прочитать. Если первый `.message()`
            // сразу `None` — это и есть случай "нет запущенного
            // backend'а", см. документацию `Daemon::stream_instance_logs`
            // за тем, почему это не ошибка и не InvalidArgument.
            let mut got_any_line = false;
            while let Some(line) = stream.message().await? {
                got_any_line = true;
                let prefix = match line.source() {
                    LogStreamSource::Stdout => "stdout",
                    LogStreamSource::Stderr => "stderr",
                    // `prost` всегда требует первый вариант enum'а как
                    // значение 0 (см. комментарий у `enum LogStreamSource`
                    // в `andler.proto`) — сервер никогда сознательно не
                    // отправляет `Unspecified` (см. `convert.rs`:
                    // `From<LogLine>` всегда вызывает `set_source` с
                    // `Stdout`/`Stderr`), но компилятор не знает об этом
                    // инварианте на уровне типов, поэтому ветка всё равно
                    // обязательна. Печатаем как есть, не падаем — это не
                    // повод обрывать стрим клиенту.
                    LogStreamSource::Unspecified => "unspecified",
                };
                println!("[{prefix}] {}", line.line);
            }

            if !got_any_line {
                eprintln!(
                    "no log lines received (instance may have no running backend right now, \
                     or simply hasn't written anything to stdout/stderr yet)"
                );
            }
        }
        Command::Metrics { instance_id } => {
            let mut stream = client
                .stream_resource_metrics(InstanceIdRequest { instance_id })
                .await?
                .into_inner();

            let mut got_any_sample = false;
            while let Some(m) = stream.message().await? {
                got_any_sample = true;
                let cpu = m
                    .cpu_percent
                    .map(|v| format!("{:.1}%", v))
                    .unwrap_or_else(|| "N/A".to_string());
                let rss = m
                    .memory_used_bytes
                    .map(|v| format_bytes(v))
                    .unwrap_or_else(|| "N/A".to_string());
                let dr = m
                    .disk_read_bytes_per_sec
                    .map(|v| format_bytes_per_sec(v))
                    .unwrap_or_else(|| "N/A".to_string());
                let dw = m
                    .disk_write_bytes_per_sec
                    .map(|v| format_bytes_per_sec(v))
                    .unwrap_or_else(|| "N/A".to_string());
                let nr = m
                    .net_rx_bytes_per_sec
                    .map(|v| format_bytes_per_sec(v))
                    .unwrap_or_else(|| "N/A".to_string());
                let nt = m
                    .net_tx_bytes_per_sec
                    .map(|v| format_bytes_per_sec(v))
                    .unwrap_or_else(|| "N/A".to_string());
                let vram = match (m.vram_used_bytes, m.vram_total_bytes) {
                    (Some(used), Some(total)) => {
                        format!("{}/{}", format_bytes(used), format_bytes(total))
                    }
                    (Some(used), None) => format_bytes(used),
                    _ => "N/A".to_string(),
                };
                let gpu_load = m
                    .gpu_load_percent
                    .map(|v| format!("{:.0}%", v))
                    .unwrap_or_else(|| "N/A".to_string());
                println!(
                    "cpu={cpu:<8} rss={rss:<10} disk_r={dr:<12} disk_w={dw:<12} \
                     net_rx={nr:<12} net_tx={nt:<12} vram={vram:<16} gpu={gpu_load:<6}"
                );
            }

            if !got_any_sample {
                eprintln!(
                    "no metrics received (instance may have no running backend right now)"
                );
            }
        }
        Command::Clone {
            source_instance_id,
            name,
            instances_root,
            mode,
        } => {
            let response = client
                .clone_instance(CloneInstanceRequest {
                    source_instance_id,
                    new_name: name,
                    instances_root,
                    mode: andler_rpc::proto::CloneMode::from(mode) as i32,
                })
                .await?
                .into_inner();
            println!("cloned instance_id={}", response.instance_id);
        }
        Command::Export {
            source_instance_id,
            dest_path,
        } => {
            let response = client
                .export_instance_disk(ExportInstanceDiskRequest {
                    source_instance_id,
                    dest_path,
                })
                .await?
                .into_inner();
            println!("exported to {}", response.dest_path);
        }
        Command::Snapshot {
            instance_id,
            action,
        } => match action {
            SnapshotAction::Create { tag, description } => {
                let response = client
                    .create_snapshot(CreateSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                        description: description.unwrap_or_default(),
                    })
                    .await?
                    .into_inner();
                println!(
                    "snapshot created: tag={}, id={}, created_at={}",
                    response.tag, response.snapshot_id, response.created_at
                );
            }
            SnapshotAction::Restore { tag } => {
                client
                    .restore_snapshot(RestoreSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                    })
                    .await?;
                println!("snapshot {} restored", tag);
            }
            SnapshotAction::Delete { tag } => {
                client
                    .delete_snapshot(DeleteSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                    })
                    .await?;
                println!("snapshot {} deleted", tag);
            }
            SnapshotAction::List => {
                let response = client
                    .list_snapshots(InstanceIdRequest { instance_id })
                    .await?
                    .into_inner();
                if response.snapshots.is_empty() {
                    println!("no snapshots");
                } else {
                    for snap in &response.snapshots {
                        println!(
                            "tag={}, id={}, created_at={}, description={}",
                            snap.tag,
                            snap.snapshot_id,
                            snap.created_at,
                            if snap.description.is_empty() {
                                "-"
                            } else {
                                &snap.description
                            }
                        );
                    }
                }
            }
        },
    }

    Ok(())
}
