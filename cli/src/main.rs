//! ANDLER CLI — thin gRPC client to `andlerd`. No business logic here:
//! each subcommand builds a single gRPC request via `andler-rpc`/`tonic`
//! and prints the response. See the crate's README.md.

mod clone;
mod create;
mod disk;
mod helpers;
mod instance_file;
mod lifecycle;
mod snapshot;
mod status;
mod wizard;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, ArmTranslator as ProtoArmTranslator,
    RootMode as ProtoRootMode,
};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

const DEFAULT_DAEMON_ADDR: &str = "http://127.0.0.1:50051";

fn default_instances_root() -> String {
    andler_core::paths::instances_root()
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

        /// Disk size in GiB (optional, default: 256). Linux only.
        #[arg(long)]
        disk_size_gib: Option<u64>,

        /// Automatically compact (qemu-img convert) the disk after every
        /// graceful shutdown. Off by default — see PLAN.md, "Disk
        /// management": compaction rewrites the whole disk file and can
        /// take noticeable time on large disks, so it must be an
        /// explicit opt-in, not silently enabled for every qcow2 disk
        /// (which is the default format). Has no effect on raw disks.
        #[arg(long)]
        compact_on_shutdown: bool,

        /// Bus for the ISO/CD-ROM drive. Linux only. Default: auto
        /// (decide by ISO filename — see CliCdromBus / PLAN.md).
        #[arg(long, value_enum, default_value = "auto")]
        cdrom_bus: CliCdromBus,

        /// Skip the interactive wizard and create with all defaults.
        /// Requires `--kind`. Mutually exclusive with `--file`.
        #[arg(long)]
        quick: bool,

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

        /// ARM->x86 translation. Omit to auto-detect from host CPU vendor
        /// (AMD -> libndk, Intel -> libhoudini) when the wizard runs;
        /// in pure CLI mode (no wizard), omitting this defaults to `none`.
        #[arg(long, value_enum)]
        arm_translator: Option<CliArmTranslator>,

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
    List {
        /// Print the full instance UUID instead of the shortened
        /// 8-character prefix shown by default (same idea as `docker
        /// ps -q`/`--no-trunc`, needed for scripts that want an
        /// unambiguous id to feed back into other commands).
        #[arg(long = "full-id", short = 'q')]
        full_id: bool,
    },
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

/// Bus for the ISO/CD-ROM drive of a Linux VM. See PLAN.md, "Монтирование
/// ISO / CD-ROM", and `andler_core::CdromBus` for the full rationale.
#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum CliCdromBus {
    /// Decide based on the ISO filename: known Linux distros get
    /// virtio-scsi (faster), anything unrecognized (Windows, unknown
    /// ISO) falls back to ide (safer, no virtio drivers needed at boot).
    #[default]
    Auto,
    /// Force virtio-scsi-pci + scsi-cd. Faster, but requires the
    /// installer environment to support virtio-scsi at early boot.
    Virtio,
    /// Force ide-cd. Slower, but works without any virtio drivers —
    /// safe choice for Windows or any ISO you're unsure about.
    Ide,
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
    /// Create a new empty qcow2 disk. Path without an extension gets
    /// `.qcow2` appended automatically; an explicit extension (.img,
    /// .raw, ...) is used as-is.
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
    /// Resize an existing disk. Growing is always allowed; shrinking
    /// requires --shrink (risk of guest data loss if the filesystem
    /// inside the guest was not shrunk first — see PLAN.md, раздел
    /// "Disk management").
    Resize {
        /// Path to the disk file.
        path: PathBuf,
        /// New size (e.g. "80GB", "512000MB", or plain bytes).
        #[arg(long)]
        size: String,
        /// Confirm shrinking the disk below its current size. Required
        /// only when the new size is smaller than the current one.
        #[arg(long)]
        shrink: bool,
    },
    /// Compact a disk (reclaim unused space). Only applicable to qcow2 —
    /// raw disks have no reclaimable metadata, see PLAN.md.
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

#[derive(Debug, Clone, Copy, ValueEnum)]
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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliArmTranslator {
    None,
    Libndk,
    Libhoudini,
}

impl From<CliArmTranslator> for ProtoArmTranslator {
    fn from(value: CliArmTranslator) -> Self {
        match value {
            CliArmTranslator::None => ProtoArmTranslator::None,
            CliArmTranslator::Libndk => ProtoArmTranslator::Libndk,
            CliArmTranslator::Libhoudini => ProtoArmTranslator::Libhoudini,
        }
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
            compact_on_shutdown,
            cdrom_bus,
            quick,
            android_version,
            base_image_path,
            gapps,
            microg,
            arm_translator,
            root,
            instances_root,
            overlay_size_gib,
            magisk_dir,
        } => {
            create::handle(
                &mut client, file, kind, name, ovmf_vars_template,
                iso_path, disk_path, disk_size_gib, compact_on_shutdown, cdrom_bus,
                quick,
                android_version, base_image_path, gapps, microg, arm_translator, root,
                instances_root, overlay_size_gib, magisk_dir,
            ).await?;
        }
        Command::Start { instance_id } => {
            lifecycle::handle_start(&mut client, instance_id).await?;
        }
        Command::Stop {
            instance_id,
            graceful,
        } => {
            lifecycle::handle_stop(&mut client, instance_id, graceful).await?;
        }
        Command::Pause { instance_id } => {
            lifecycle::handle_pause(&mut client, instance_id).await?;
        }
        Command::Resume { instance_id } => {
            lifecycle::handle_resume(&mut client, instance_id).await?;
        }
        Command::Status { instance_id } => {
            status::handle_status(&mut client, instance_id).await?;
        }
        Command::List { full_id } => {
            status::handle_list(&mut client, full_id).await?;
        }
        Command::Remove { instance_id, purge } => {
            lifecycle::handle_remove(&mut client, instance_id, purge).await?;
        }
        Command::Config { instance_id } => {
            status::handle_config(&mut client, instance_id).await?;
        }
        Command::Logs { instance_id } => {
            status::handle_logs(&mut client, instance_id).await?;
        }
        Command::Metrics { instance_id } => {
            status::handle_metrics(&mut client, instance_id).await?;
        }
        Command::Clone {
            source_instance_id,
            name,
            instances_root,
            mode,
        } => {
            clone::handle_clone(&mut client, source_instance_id, name, instances_root, mode).await?;
        }
        Command::Export {
            source_instance_id,
            dest_path,
        } => {
            clone::handle_export(&mut client, source_instance_id, dest_path).await?;
        }
        Command::Snapshot {
            instance_id,
            action,
        } => {
            snapshot::handle(&mut client, instance_id, action).await?;
        }
        Command::Disk { action } => {
            disk::handle(action).await?;
        }
    }

    Ok(())
}
