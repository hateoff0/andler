//! ANDLER CLI — thin gRPC client to `andlerd`. No business logic here:
//! each subcommand builds a single gRPC request via `andler-rpc`/`tonic`
//! and prints the response. See the crate's README.md.

mod instance_file;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    instance_kind, network_mode, render_backend, AndroidProfile as ProtoAndroidProfile,
    AndroidVersion as ProtoAndroidVersion, AudioBackend, BackendKind, CloneInstanceRequest,
    CpuPriority, CreateAndroidInstanceRequest, CreateInstanceRequest, CreateSnapshotRequest,
    DeleteSnapshotRequest, DiskFormat, DisplayEngine, Empty, ExportInstanceDiskRequest,
    GetInstanceConfigResponse, InstanceIdRequest, InstanceStateKind, LogStreamSource,
    RemoveInstanceRequest, RestoreSnapshotRequest, RootMode as ProtoRootMode, StopInstanceRequest,
};
use clap::{Parser, Subcommand, ValueEnum};
use instance_file::{InstanceFile, InstanceFileResult};
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
#[command(
    name = "andler",
    about = "Thin CLI client to andlerd",
    long_about = "ANDLER CLI — create, manage and monitor Android/Linux VMs.\n\n\
                   Communicates with andlerd over gRPC (default: http://127.0.0.1:50051).\n\
                   Set ANDLERD_ADDR env var or use --daemon-addr to override."
)]
struct Cli {
    /// andlerd address. Defaults to $ANDLERD_ADDR or http://127.0.0.1:50051.
    #[arg(long, global = true)]
    daemon_addr: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new VM instance.
    ///
    /// Two modes:
    ///
    /// TOML mode (--file): create from a config file (LinuxVm or AndroidVm,
    /// auto-detected by content). Example:
    ///   andler create --file instance.toml
    ///
    /// CLI mode (--kind): create via flags. --kind selects the VM type.
    /// LinuxVm example:
    ///   andler create --kind linux --name my-vm \
    ///     --iso-path /path/to/installer.iso \
    ///     --disk-path /path/to/disk.qcow2 \
    ///     --ovmf-vars-template /path/to/VARS.fd
    ///
    /// AndroidVm example:
    ///   andler create --kind android --name my-android \
    ///     --android-version 13 \
    ///     --base-image-path /path/to/base.qcow2 \
    ///     --ovmf-vars-template /path/to/VARS.fd
    Create {
        /// Path to TOML config file (TOML mode). Mutually exclusive with --kind.
        #[arg(long)]
        file: Option<PathBuf>,

        // --- CLI mode ---

        /// VM type selector: "linux" or "android". Enables CLI mode
        /// where you specify all parameters as flags instead of TOML.
        #[arg(long)]
        kind: Option<CliKind>,

        /// Instance name (required in CLI mode).
        #[arg(long)]
        name: Option<String>,

        /// OVMF VARS template path (required in CLI mode).
        #[arg(long)]
        ovmf_vars_template: Option<String>,

        // --- Linux-specific (required when --kind linux) ---

        /// Path to installer ISO (required for --kind linux).
        #[arg(long)]
        iso_path: Option<String>,

        /// Path to disk file (required for --kind linux).
        #[arg(long)]
        disk_path: Option<String>,

        /// Disk size in GiB (optional, default: 40). Linux only.
        #[arg(long)]
        disk_size_gib: Option<u64>,

        // --- Android-specific (required when --kind android) ---

        /// Android version (required for --kind android).
        #[arg(long, value_enum)]
        android_version: Option<CliAndroidVersion>,

        /// Path to Android base image (required for --kind android).
        #[arg(long)]
        base_image_path: Option<String>,

        /// Include Google Apps.
        #[arg(long)]
        gapps: bool,

        /// Include microG.
        #[arg(long)]
        microg: bool,

        /// Include ARM->x86 translation (libhoudini/libndk).
        #[arg(long)]
        libndk: bool,

        /// Root mode: none or magisk.
        #[arg(long, value_enum, default_value_t = CliRootMode::None)]
        root: CliRootMode,

        /// Instance directory root (default: ~/.local/share/andler/instances).
        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,

        /// Overlay disk size in GiB (default: 20). Android only.
        #[arg(long, default_value_t = 20)]
        overlay_size_gib: u64,

        /// Path to Magisk binaries directory (required when --root magisk).
        #[arg(long)]
        magisk_dir: Option<PathBuf>,
    },
    /// Start a previously created instance.
    Start { instance_id: String },
    /// Stop a running instance.
    Stop {
        instance_id: String,
        /// Force stop without waiting for graceful shutdown.
        #[arg(long)]
        graceful: bool,
    },
    /// Pause a running instance.
    Pause { instance_id: String },
    /// Resume a paused instance.
    Resume { instance_id: String },
    /// Print current instance status.
    Status { instance_id: String },
    /// List all registered instances (id / name / state).
    List,
    /// Remove an instance record. Instance must be stopped first.
    /// Without --purge, only removes the record; with --purge, also
    /// deletes disk and OVMF VARS files.
    Remove {
        instance_id: String,
        /// Also delete instance files from disk.
        #[arg(long)]
        purge: bool,
    },
    /// Print full instance configuration (all sections).
    Config { instance_id: String },
    /// Stream stdout/stderr from the instance's hypervisor process.
    Logs { instance_id: String },
    /// Stream resource metrics (CPU%, RAM, disk I/O, net I/O, GPU) in real time.
    Metrics { instance_id: String },
    /// Clone an instance into a new independent instance.
    Clone {
        source_instance_id: String,
        /// Name for the new instance.
        #[arg(long)]
        name: String,
        /// Instance directory root for clone files.
        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,
        /// Clone mode: linked, full-standalone, or shared-base (Android only).
        #[arg(long, value_enum)]
        mode: CliCloneMode,
    },
    /// Export instance disk to a standalone file for transfer/backup.
    Export {
        source_instance_id: String,
        dest_path: String,
    },
    /// Manage instance snapshots (create/restore/delete/list).
    Snapshot {
        instance_id: String,
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// Disk management operations (create/info/resize/compact).
    Disk {
        #[command(subcommand)]
        action: DiskAction,
    },
}

/// VM type selector for CLI mode.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliKind {
    /// Linux VM — requires --iso-path, --disk-path, --ovmf-vars-template.
    Linux,
    /// Android VM — requires --android-version, --base-image-path, --ovmf-vars-template.
    Android,
}

/// Snapshot subcommands.
#[derive(Subcommand)]
enum SnapshotAction {
    /// Create a snapshot of the current state (requires Running/Paused).
    Create {
        #[arg(long)]
        tag: String,
        #[arg(long)]
        description: Option<String>,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Restore from a snapshot (requires stopped instance).
    Restore {
        #[arg(long)]
        tag: String,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Delete a snapshot (requires stopped instance).
    Delete {
        #[arg(long)]
        tag: String,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// List all snapshots.
    List,
}

/// Disk management subcommands.
#[derive(Subcommand)]
enum DiskAction {
    /// Create a new empty qcow2 disk.
    Create {
        /// Path for the new disk file.
        path: PathBuf,
        /// Disk size (e.g. "64GB", "128000MB", "1T", or plain bytes).
        #[arg(long)]
        size: String,
    },
    /// Show disk information (virtual size, actual usage, format).
    Info {
        /// Path to the disk file.
        path: PathBuf,
    },
    /// Resize an existing disk.
    Resize {
        /// Path to the disk file.
        path: PathBuf,
        /// New size (e.g. "80GB", "512000MB", or plain bytes).
        #[arg(long)]
        size: String,
    },
    /// Compact a disk (reclaim unused space).
    Compact {
        /// Path to the disk file.
        path: PathBuf,
    },
}

/// Clone modes — maps to `andler_core::CloneMode` one-to-one.
#[derive(Clone, Copy, ValueEnum)]
enum CliCloneMode {
    /// Cheap, fast; clone depends on source.
    Linked,
    /// Fully standalone; independent but more expensive.
    #[value(name = "full-standalone")]
    FullStandalone,
    /// Thin relative to shared profile base image.
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

/// Format bytes to human-readable (KB/MB/GB).
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

/// Format bytes/sec to human-readable.
fn format_bytes_per_sec(bps: u64) -> String {
    format!("{}/s", format_bytes(bps))
}

/// Parse a human-readable size string into bytes.
///
/// Supports: `64GB`, `64gb`, `64 G`, `64GiB`, `128000MB`, `1T`, `1TiB`,
/// or plain number (bytes). Space between number and unit is optional.
/// Case-insensitive.
fn parse_size(input: &str) -> Result<u64, String> {
    let input = input.trim().to_uppercase().replace(' ', "");
    if input.is_empty() {
        return Err("empty size string".to_string());
    }

    // Find where digits end and unit begins
    let split = input
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(input.len());

    let number_part = &input[..split];
    let unit_part = &input[split..];

    if number_part.is_empty() {
        return Err(format!("missing number before `{unit_part}`"));
    }
    if unit_part.starts_with('.') {
        return Err(format!(
            "decimal sizes not supported (use e.g. 64GB, not 1.5GB)"
        ));
    }

    let number: u64 = number_part
        .parse()
        .map_err(|e| format!("invalid number `{number_part}`: {e}"))?;

    let bytes = match unit_part {
        "" => number,
        "B" => number,
        "KB" | "KIB" | "K" => number.checked_mul(1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "MB" | "MIB" | "M" => number.checked_mul(1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "GB" | "GIB" | "G" => number.checked_mul(1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "TB" | "TIB" | "T" => number.checked_mul(1024 * 1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        _ => return Err(format!(
            "unknown unit `{unit_part}` (use B, KB/KiB, MB/MiB, GB/GiB, TB/TiB)"
        )),
    };

    Ok(bytes)
}

/// Format bytes to human-readable size string (e.g. "40.0 GiB").
fn format_size(bytes: u64) -> String {
    const TIB: u64 = 1024 * 1024 * 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;

    if bytes > 0 && bytes % TIB == 0 {
        format!("{} TiB", bytes / TIB)
    } else if bytes > 0 && bytes % GIB == 0 {
        format!("{} GiB", bytes / GIB)
    } else if bytes > 0 && bytes % MIB == 0 {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Print `GetInstanceConfigResponse` in a human-readable format.
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

/// Build `CreateInstanceRequest` from CLI flags (LinuxVm).
fn build_linux_request(
    name: String,
    iso_path: String,
    disk_path: String,
    disk_size_gib: Option<u64>,
    ovmf_vars_template: String,
) -> CreateInstanceRequest {
    let mut disk = andler_core::DiskConfig::reference_default(std::path::PathBuf::from(&disk_path));
    if let Some(gib) = disk_size_gib {
        disk.size_bytes = gib.checked_mul(andler_core::DiskConfig::GIB)
            .expect("disk size overflow");
    }

    CreateInstanceRequest {
        name,
        iso_path,
        cpu: Some(andler_core::CpuConfig::reference_default().into()),
        memory: Some(andler_core::MemoryConfig::reference_default().into()),
        disk: Some(disk.into()),
        display: Some(andler_core::DisplayConfig::reference_default().into()),
        gpu: Some(andler_core::GpuConfig::reference_default().into()),
        network: Some(andler_core::NetworkConfig::reference_default().into()),
        firmware: Some(
            andler_core::FirmwareConfig::reference_default(std::path::PathBuf::from(
                &ovmf_vars_template,
            ))
            .into(),
        ),
        audio: Some(andler_core::AudioConfig::reference_default().into()),
        input: Some(andler_core::InputConfig::reference_default().into()),
    }
}

/// Build `CreateAndroidInstanceRequest` from CLI flags.
fn build_android_request(
    name: String,
    android_version: CliAndroidVersion,
    base_image_path: String,
    ovmf_vars_template: String,
    gapps: bool,
    microg: bool,
    libndk: bool,
    root: CliRootMode,
    instances_root: String,
    overlay_size_gib: u64,
    magisk_dir: Option<PathBuf>,
) -> CreateAndroidInstanceRequest {
    let mut profile = ProtoAndroidProfile {
        gapps,
        microg,
        libndk,
        ..Default::default()
    };
    profile.set_android_version(android_version.into());
    profile.set_root(root.into());

    CreateAndroidInstanceRequest {
        name,
        profile: Some(profile),
        base_image_path,
        instances_root,
        overlay_size_bytes: overlay_size_gib.checked_mul(1024 * 1024 * 1024)
            .expect("overlay size overflow"),
        ovmf_vars_template,
        magisk_dir: magisk_dir
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn err_exit(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
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
            kind,
            name,
            ovmf_vars_template,
            iso_path,
            disk_path,
            disk_size_gib,
            android_version,
            base_image_path,
            gapps,
            microg,
            libndk,
            root,
            instances_root,
            overlay_size_gib,
            magisk_dir,
        } => {
            let has_file = file.is_some();
            let has_kind = kind.is_some();

            if has_file && has_kind {
                err_exit("error: --file and --kind are mutually exclusive");
            }
            if !has_file && !has_kind {
                err_exit("error: specify either --file <path> or --kind linux|android");
            }

            if has_file {
                // --- TOML mode: auto-detect Linux/Android ---
                let file = file.unwrap();
                let instance_file = InstanceFile::load(&file)?;
                match instance_file.into_result() {
                    InstanceFileResult::Linux(req) => {
                        let response = client.create_instance(req).await?;
                        println!("{}", response.into_inner().instance_id);
                    }
                    InstanceFileResult::Android(req) => {
                        let response = client.create_android_instance(req).await?;
                        println!("{}", response.into_inner().instance_id);
                    }
                }
            } else {
                // --- CLI mode: validate required flags per kind ---
                let kind = kind.unwrap();
                let name = name.unwrap_or_else(|| err_exit("error: --name is required"));
                let ovmf = ovmf_vars_template
                    .unwrap_or_else(|| err_exit("error: --ovmf-vars-template is required"));

                match kind {
                    CliKind::Linux => {
                        let iso = iso_path
                            .unwrap_or_else(|| err_exit("error: --iso-path is required for --kind linux"));
                        let disk = disk_path
                            .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));

                        let req = build_linux_request(name, iso, disk, disk_size_gib, ovmf);
                        let response = client.create_instance(req).await?;
                        println!("{}", response.into_inner().instance_id);
                    }
                    CliKind::Android => {
                        let av = android_version.unwrap_or_else(|| {
                            err_exit("error: --android-version is required for --kind android")
                        });
                        let bip = base_image_path.unwrap_or_else(|| {
                            err_exit("error: --base-image-path is required for --kind android")
                        });

                        if root == CliRootMode::Magisk && magisk_dir.is_none() {
                            err_exit("error: --magisk-dir is required when --root magisk");
                        }

                        let req = build_android_request(
                            name, av, bip, ovmf, gapps, microg, libndk, root,
                            instances_root, overlay_size_gib, magisk_dir,
                        );
                        let response = client.create_android_instance(req).await?;
                        println!("{}", response.into_inner().instance_id);
                    }
                }
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

            let mut got_any_line = false;
            while let Some(line) = stream.message().await? {
                got_any_line = true;
                let prefix = match line.source() {
                    LogStreamSource::Stdout => "stdout",
                    LogStreamSource::Stderr => "stderr",
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
            SnapshotAction::Create { tag, description, timeout } => {
                let response = client
                    .create_snapshot(CreateSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                        description: description.unwrap_or_default(),
                        timeout_secs: timeout,
                    })
                    .await?
                    .into_inner();
                println!(
                    "snapshot created: tag={}, id={}, created_at={}",
                    response.tag, response.snapshot_id, response.created_at
                );
            }
            SnapshotAction::Restore { tag, timeout } => {
                client
                    .restore_snapshot(RestoreSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                        timeout_secs: timeout,
                    })
                    .await?;
                println!("snapshot {} restored", tag);
            }
            SnapshotAction::Delete { tag, timeout } => {
                client
                    .delete_snapshot(DeleteSnapshotRequest {
                        instance_id,
                        tag: tag.clone(),
                        timeout_secs: timeout,
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
        Command::Disk { action } => match action {
            DiskAction::Create { path, size } => {
                let bytes = parse_size(&size)?;
                andler_disk::qcow2::create(&path, bytes).await?;
                println!("created {}", path.display());
            }
            DiskAction::Info { path } => {
                let info = andler_disk::qcow2::info(&path).await?;
                println!("path:         {}", path.display());
                println!("format:       {}", info.format);
                println!("virtual_size: {}", format_size(info.virtual_size));
                let pct = if info.virtual_size > 0 {
                    info.actual_size as f64 / info.virtual_size as f64 * 100.0
                } else {
                    0.0
                };
                println!(
                    "actual_usage: {} ({:.1}%)",
                    format_size(info.actual_size),
                    pct
                );
                match info.backing_file {
                    Some(bf) => println!("backing_file: {bf}"),
                    None => println!("backing_file: none"),
                }
            }
            DiskAction::Resize { path, size } => {
                let bytes = parse_size(&size)?;
                andler_disk::qcow2::resize(&path, bytes).await?;
                println!("resized {} to {}", path.display(), format_size(bytes));
            }
            DiskAction::Compact { path } => {
                andler_disk::qcow2::compact(&path).await?;
                println!("compacted {}", path.display());
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_size tests ---

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("512000").unwrap(), 512000);
    }

    #[test]
    fn parse_size_kb() {
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1kb").unwrap(), 1024);
        assert_eq!(parse_size("1kib").unwrap(), 1024);
        assert_eq!(parse_size("1 K").unwrap(), 1024);
    }

    #[test]
    fn parse_size_mb() {
        assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("100mb").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("128000MiB").unwrap(), 128000 * 1024 * 1024);
        assert_eq!(parse_size("1 M").unwrap(), 1024 * 1024);
    }

    #[test]
    fn parse_size_gb() {
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("64gb").unwrap(), 64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("64GiB").unwrap(), 64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("40 G").unwrap(), 40 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_tb() {
        assert_eq!(parse_size("1TB").unwrap(), 1024u64 * 1024 * 1024 * 1024);
        assert_eq!(
            parse_size("1T").unwrap(),
            1024u64 * 1024 * 1024 * 1024
        );
        assert_eq!(
            parse_size("2TiB").unwrap(),
            2 * 1024u64 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn parse_size_with_spaces() {
        assert_eq!(parse_size("64 GB").unwrap(), 64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1 TB").unwrap(), 1024u64 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_errors() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("64XB").is_err());
        assert!(parse_size("-1GB").is_err());
    }

    #[test]
    fn parse_size_overflow_returns_error() {
        assert!(parse_size("99999999999TB").is_err());
        assert!(parse_size("18446744073709551616GB").is_err());
    }

    #[test]
    fn parse_size_empty_number_is_error() {
        let err = parse_size("GB").unwrap_err();
        assert!(err.contains("missing number"));
    }

    #[test]
    fn parse_size_float_is_error() {
        let err = parse_size("1.5GB").unwrap_err();
        assert!(err.contains("decimal"));
    }

    #[test]
    fn parse_size_one_byte() {
        assert_eq!(parse_size("1B").unwrap(), 1);
        assert_eq!(parse_size("1").unwrap(), 1);
    }

    #[test]
    fn parse_size_whitespace_trimmed() {
        assert_eq!(parse_size(" 64GB ").unwrap(), 64 * 1024 * 1024 * 1024);
    }

    // --- format_size tests ---

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
    }

    #[test]
    fn format_size_kib() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
    }

    #[test]
    fn format_size_mib() {
        assert_eq!(format_size(1024 * 1024), "1 MiB");
        assert_eq!(format_size(1024 * 1024 * 5), "5 MiB");
    }

    #[test]
    fn format_size_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1 GiB");
        assert_eq!(format_size(1024 * 1024 * 1024 * 40), "40 GiB");
    }

    #[test]
    fn format_size_tib() {
        assert_eq!(format_size(1024u64 * 1024 * 1024 * 1024), "1 TiB");
        assert_eq!(format_size(1024u64 * 1024 * 1024 * 1024 * 4), "4 TiB");
    }

    #[test]
    fn format_size_below_kib() {
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_fractional_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024 + 1), "1.0 GiB");
    }
}
