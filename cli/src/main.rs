//! ANDLER CLI — thin gRPC client to `andlerd`. No business logic here:
//! each subcommand builds a single gRPC request via `andler-rpc`/`tonic`
//! and prints the response. See the crate's README.md.

mod clone;
mod create;
mod disk;
mod edit;
mod guest;
mod helpers;
mod instance_file;
mod lifecycle;
mod preview;
mod snapshot;
mod status;
mod verify;
mod wizard;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, ArmTranslator as ProtoArmTranslator,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
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
    command: Option<Command>,
}

// --- Dual-syntax args structs for simple commands ---

macro_rules! dual_id_args {
    ($name:ident) => {
        #[derive(Args)]
        pub struct $name {
            /// Instance ID (positional form)
            pub instance_id: Option<String>,
            /// Instance ID (flag form)
            #[arg(long, short)]
            pub instance: Option<String>,
        }

        impl $name {
            pub fn resolve_id(&self) -> Result<&str, clap::Error> {
                match (&self.instance_id, &self.instance) {
                    (Some(id), None) | (None, Some(id)) => Ok(id),
                    (Some(_), Some(_)) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                        "specify instance ID once: positional or --instance",
                    )),
                    (None, None) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                        "instance ID required: positional or --instance",
                    )),
                }
            }
        }
    };
}

dual_id_args!(StartArgs);
dual_id_args!(PauseArgs);
dual_id_args!(ResumeArgs);
dual_id_args!(StatusArgs);

#[derive(Args)]
pub struct StopArgs {
    /// Instance ID (positional form)
    pub instance_id: Option<String>,
    /// Instance ID (flag form)
    #[arg(long, short)]
    pub instance: Option<String>,
    /// Force stop without waiting for graceful shutdown.
    #[arg(long)]
    pub graceful: bool,
}

impl StopArgs {
    pub fn resolve_id(&self) -> Result<&str, clap::Error> {
        match (&self.instance_id, &self.instance) {
            (Some(id), None) | (None, Some(id)) => Ok(id),
            (Some(_), Some(_)) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "specify instance ID once: positional or --instance",
            )),
            (None, None) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "instance ID required: positional or --instance",
            )),
        }
    }
}

#[derive(Args)]
pub struct RemoveArgs {
    /// Instance ID (positional form)
    pub instance_id: Option<String>,
    /// Instance ID (flag form)
    #[arg(long, short)]
    pub instance: Option<String>,
    /// Also delete instance files from disk.
    #[arg(long)]
    pub purge: bool,
}

impl RemoveArgs {
    pub fn resolve_id(&self) -> Result<&str, clap::Error> {
        match (&self.instance_id, &self.instance) {
            (Some(id), None) | (None, Some(id)) => Ok(id),
            (Some(_), Some(_)) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "specify instance ID once: positional or --instance",
            )),
            (None, None) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "instance ID required: positional or --instance",
            )),
        }
    }
}

#[derive(Args)]
pub struct LogsArgs {
    /// Instance ID (positional form)
    pub instance_id: Option<String>,
    /// Instance ID (flag form)
    #[arg(long, short)]
    pub instance: Option<String>,
    /// Only show lines from this stream.
    #[arg(long, value_enum)]
    pub(crate) source: Option<CliLogSource>,
    /// Only show lines matching this regular expression.
    #[arg(long)]
    pub grep: Option<String>,
    /// Limit how much backlog is shown before continuing to follow
    /// live output, instead of the full history.
    #[arg(long)]
    pub tail: Option<usize>,
}

impl LogsArgs {
    pub fn resolve_id(&self) -> Result<&str, clap::Error> {
        match (&self.instance_id, &self.instance) {
            (Some(id), None) | (None, Some(id)) => Ok(id),
            (Some(_), Some(_)) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "specify instance ID once: positional or --instance",
            )),
            (None, None) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "instance ID required: positional or --instance",
            )),
        }
    }
}

#[derive(Args)]
pub struct MetricsArgs {
    /// Instance ID (positional form)
    pub instance_id: Option<String>,
    /// Instance ID (flag form)
    #[arg(long, short)]
    pub instance: Option<String>,
    /// Print a single sample and exit, instead of streaming
    /// continuously.
    #[arg(long)]
    pub once: bool,
    /// Machine-readable JSON output (one object per sample).
    #[arg(long)]
    pub json: bool,
}

impl MetricsArgs {
    pub fn resolve_id(&self) -> Result<&str, clap::Error> {
        match (&self.instance_id, &self.instance) {
            (Some(id), None) | (None, Some(id)) => Ok(id),
            (Some(_), Some(_)) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "specify instance ID once: positional or --instance",
            )),
            (None, None) => Err(clap::Error::raw(clap::error::ErrorKind::InvalidValue,
                "instance ID required: positional or --instance",
            )),
        }
    }
}

// --- Config subcommand and flags ---

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Read-only view (default when no subcommand)
    View {
        /// Instance ID (positional form)
        instance_id: Option<String>,
    },
    /// Edit in $EDITOR
    Edit {
        /// Instance ID (positional form)
        instance_id: Option<String>,
    },
    /// Set a config property (key=value)
    Set {
        /// Instance ID
        instance_id: String,
        /// Config key
        key: String,
        /// Config value
        value: String,
    },
}

#[derive(Args)]
pub struct ConfigFlags {
    /// Instance ID (flag form for view/edit without subcommand)
    #[arg(long, short)]
    pub instance: Option<String>,
    /// Edit in $EDITOR (flag form)
    #[arg(long, short = 'e')]
    pub edit: bool,
    /// Apply config from TOML file
    #[arg(long, short)]
    pub file: Option<PathBuf>,
}

// --- Disk flags (flag-form alternative to subcommands) ---

#[derive(Args)]
pub struct DiskFlags {
    /// Create a new disk
    #[arg(long, short = 'c')]
    pub create: bool,
    /// Show disk info
    #[arg(long, short = 'i')]
    pub info: bool,
    /// Resize disk
    #[arg(long, short = 'r')]
    pub resize: bool,
    /// Compact disk
    #[arg(long, short = 'm')]
    pub compact: bool,
    /// Disk file path
    #[arg(long, short = 'p')]
    pub path: Option<String>,
    /// Disk size
    #[arg(long, short = 's')]
    pub size: Option<String>,
    /// Confirm shrinking
    #[arg(long)]
    pub shrink: bool,
}

// --- Command enum ---

#[derive(Subcommand)]
enum Command {
    /// Create a new VM instance.
    Create {
        /// Path to TOML config file (TOML mode). Mutually exclusive with --kind.
        #[arg(long)]
        file: Option<PathBuf>,
        // --- CLI mode ---
        /// VM type selector: "linux" or "android".
        #[arg(long)]
        kind: Option<CliKind>,
        /// Instance name (required in CLI mode).
        #[arg(long)]
        name: Option<String>,
        /// OVMF VARS template path (required in CLI mode).
        #[arg(long)]
        ovmf_vars_template: Option<String>,
        // --- Linux-specific ---
        /// Path to installer ISO (required for --kind linux).
        #[arg(long)]
        iso_path: Option<String>,
        /// Path to disk file (required for --kind linux).
        #[arg(long)]
        disk_path: Option<String>,
        /// Disk size in GiB (optional, default: 256). Linux only.
        #[arg(long)]
        disk_size_gib: Option<u64>,
        /// Automatically compact the disk after every graceful shutdown.
        #[arg(long)]
        compact_on_shutdown: bool,
        /// Bus for the ISO/CD-ROM drive. Linux only.
        #[arg(long, value_enum, default_value = "auto")]
        cdrom_bus: CliCdromBus,
        /// Disable UEFI/OVMF, use legacy BIOS instead. Linux only.
        #[arg(long)]
        no_uefi: bool,
        /// Skip the interactive wizard and create with all defaults.
        #[arg(long)]
        quick: bool,
        /// Print the resolved config without creating.
        #[arg(long)]
        dry_run: bool,
        /// Validate the resolved config and print a pass/fail report.
        #[arg(long)]
        verify: bool,
        // --- Android-specific ---
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
        /// ARM->x86 translation.
        #[arg(long, value_enum)]
        arm_translator: Option<CliArmTranslator>,
        /// Instance directory root (default: ~/.andler/instances).
        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,
        /// Overlay disk size in GiB (default: 20). Android only.
        #[arg(long, default_value_t = 20)]
        overlay_size_gib: u64,
    },
    /// Start a previously created instance.
    ///
    /// Supports both positional and flag forms:
    ///   andler start my-vm
    ///   andler start --instance my-vm
    Start(StartArgs),
    /// Stop a running instance.
    ///
    /// Supports both positional and flag forms:
    ///   andler stop my-vm
    ///   andler stop --instance my-vm
    Stop(StopArgs),
    /// Pause a running instance.
    ///
    /// Supports both positional and flag forms:
    ///   andler pause my-vm
    ///   andler pause --instance my-vm
    Pause(PauseArgs),
    /// Resume a paused instance.
    ///
    /// Supports both positional and flag forms:
    ///   andler resume my-vm
    ///   andler resume --instance my-vm
    Resume(ResumeArgs),
    /// Print current instance status.
    ///
    /// Supports both positional and flag forms:
    ///   andler status my-vm
    ///   andler status --instance my-vm
    Status(StatusArgs),
    /// List all registered instances (id / name / state).
    List {
        /// Print the full instance UUID instead of the shortened
        /// 8-character prefix.
        #[arg(long = "full-id", short = 'q')]
        full_id: bool,
        /// Only show instances in this state (case-insensitive).
        #[arg(long)]
        state: Option<String>,
        /// Only show instances whose name matches this regex.
        #[arg(long)]
        name: Option<String>,
        /// Sort order.
        #[arg(long, value_enum, default_value = "none")]
        sort: ListSortKey,
        /// Machine-readable JSON array output.
        #[arg(long)]
        json: bool,
    },
    /// Remove an instance record. Instance must be stopped first.
    ///
    /// Supports both positional and flag forms:
    ///   andler remove my-vm --purge
    ///   andler remove --instance my-vm --purge
    Remove(RemoveArgs),
    /// Print full instance configuration or edit it.
    ///
    /// Subcommand form:
    ///   andler config view <id>
    ///   andler config edit <id>
    ///   andler config set <id> key=value
    ///
    /// Flag form:
    ///   andler config --instance <id>
    ///   andler config --instance <id> --edit
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
        #[command(flatten)]
        flags: ConfigFlags,
    },
    /// Stream stdout/stderr from the instance's hypervisor process.
    ///
    /// Supports both positional and flag forms:
    ///   andler logs my-vm --tail 50
    ///   andler logs --instance my-vm --tail 50
    Logs(LogsArgs),
    /// Stream resource metrics (CPU%, RAM, disk I/O, net I/O, GPU).
    ///
    /// Supports both positional and flag forms:
    ///   andler metrics my-vm --once
    ///   andler metrics --instance my-vm --once
    Metrics(MetricsArgs),
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
    ///
    /// Instance ID is always positional:
    ///   andler snapshot create my-vm my-snap
    ///   andler snapshot list my-vm
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// Disk management operations (create/info/resize/compact).
    ///
    /// Subcommand form:
    ///   andler disk create disk.qcow2 256G
    ///   andler disk info disk.qcow2
    ///
    /// Flag form:
    ///   andler disk --create --path disk.qcow2 --size 256G
    ///   andler disk --info --path disk.qcow2
    Disk {
        #[command(subcommand)]
        action: Option<DiskAction>,
        #[command(flatten)]
        flags: DiskFlags,
    },
    /// Guest agent operations (install/remove packages in guest OS).
    ///
    /// Instance ID is always positional:
    ///   andler guest list
    ///   andler guest list my-android
    ///   andler guest install libndk my-android
    ///   andler guest remove spice-vdagent my-android
    Guest {
        #[command(subcommand)]
        action: guest::GuestAction,
    },
    /// Launch the interactive wizard to create a new VM.
    /// This is the default when `andler` is invoked without a subcommand.
    Wizard {},
    /// Generate a shell completion script (printed to stdout).
    ///
    /// Does not require andlerd to be running. Example:
    ///   andler completions bash > ~/.bash_completions/andler.bash
    ///   andler completions zsh > ~/.zfunc/_andler
    Completions {
        /// Shell to generate completions for.
        shell: clap_complete::Shell,
    },
}

/// VM type selector for CLI mode.
/// `--source` filter for `andler logs`. See PLAN.md, item 17, "Logs
/// filtering and tail".
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliLogSource {
    Stdout,
    Stderr,
}

/// Sort key for `andler list --sort`. See PLAN.md, item 16, "List
/// filtering and sorting" — "by created date" isn't included because
/// andlerd doesn't track instance creation time anywhere today (neither
/// in `InstanceRecord` nor over the wire in `InstanceListEntry`);
/// adding it would mean a proto/persistence change, not just a CLI flag,
/// so it's left out rather than silently sorting by something else
/// under that name.
#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum ListSortKey {
    /// Whatever order `andlerd` returns them in (registration order in
    /// its in-memory map — not guaranteed stable across restarts).
    #[default]
    None,
    Name,
    State,
}

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

/// Snapshot subcommands. Instance ID is always the first positional arg.
#[derive(Subcommand)]
enum SnapshotAction {
    /// Create a snapshot of the current state (requires Running/Paused).
    Create {
        /// Instance ID
        instance_id: String,
        /// Snapshot tag
        #[arg(long)]
        tag: String,
        /// Snapshot description
        #[arg(long)]
        description: Option<String>,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Restore from a snapshot (requires Running/Paused).
    Restore {
        /// Instance ID
        instance_id: String,
        #[arg(long)]
        tag: String,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Delete a snapshot (requires Running/Paused).
    Delete {
        /// Instance ID
        instance_id: String,
        #[arg(long)]
        tag: String,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// List all snapshots for an instance.
    List {
        /// Instance ID
        instance_id: String,
    },
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

/// Formats a `tonic::Status` from a failed gRPC call into a short,
/// human-readable message instead of the raw `Debug` output (which
/// includes the full `MetadataMap`, headers, etc.) — see PLAN.md, item
/// 6, "Error message formatting (CLI-side)".
fn format_grpc_error(err: &tonic::Status) -> String {
    let msg = err.message().trim_matches('"');
    match err.code() {
        tonic::Code::NotFound => format!("instance not found: {msg}"),
        tonic::Code::InvalidArgument => format!("invalid argument: {msg}"),
        tonic::Code::FailedPrecondition => format!("cannot perform operation: {msg}"),
        tonic::Code::AlreadyExists => format!("already exists: {msg}"),
        tonic::Code::Unimplemented => format!("not supported: {msg}"),
        tonic::Code::Internal => format!("internal error: {msg}"),
        _ => format!("error: {msg}"),
    }
}

/// `true` if `err` (or anything in its `.source()` chain) looks like a
/// failure to even reach `andlerd` (connection refused, transport-level
/// error) rather than a successfully-delivered RPC that the daemon
/// itself rejected. Matched on message text, not error type: by the
/// time an error reaches `main()` here it's already erased into
/// `Box<dyn Error>`, and the actual "connection refused" wording from
/// the OS may only show up a few `.source()` levels down inside
/// `tonic::transport::Error` (whose own top-level `Display` is
/// typically a generic "transport error") rather than in `err`'s own
/// message — walking the whole chain avoids depending on exactly which
/// level tonic puts it at. See PLAN.md, item 7, "'Daemon not running'
/// friendly error".
fn looks_like_daemon_not_running(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        let message = e.to_string().to_ascii_lowercase();
        if message.contains("connection refused")
            || message.contains("transport error")
            || message.contains("os error 111")
        {
            return true;
        }
        current = e.source();
    }
    false
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let addr = std::env::var("ANDLERD_ADDR").unwrap_or_else(|_| DEFAULT_DAEMON_ADDR.to_string());
    // `--daemon-addr`, if passed, overrides this for the error message
    // below too — re-parsing here (rather than threading the already
    // -parsed `Cli` out of `run()`) keeps `run()`'s signature and
    // internal `?`-based error flow completely unchanged; this is
    // purely about what `main()` prints if `run()` fails.
    let addr = Cli::try_parse()
        .ok()
        .and_then(|cli| cli.daemon_addr)
        .unwrap_or(addr);

    if let Err(err) = run().await {
        if let Some(status) = err.downcast_ref::<tonic::Status>() {
            eprintln!("{}", format_grpc_error(status));
        } else if looks_like_daemon_not_running(err.as_ref()) {
            eprintln!("andlerd is not running at {addr}.\nStart it with: andlerd");
        } else {
            // Anything else (config file errors, IO errors building the
            // request, etc.) — not gRPC-shaped, so there's no status
            // code/connection-refused pattern to clean up; the error's
            // own Display is already meant to be read by a human (see
            // e.g. `WizardError`/`InstanceFileError`), so just print it
            // as-is rather than inventing a generic wrapper message.
            eprintln!("Error: {err}");
        }
        return std::process::ExitCode::FAILURE;
    }

    std::process::ExitCode::SUCCESS
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handled before connecting to `andlerd` at all: generating a
    // completion script is a pure, local, offline operation.
    if let Some(Command::Completions { shell }) = &cli.command {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(*shell, &mut cmd, bin_name, &mut std::io::stdout());
        return Ok(());
    }

    let addr = cli
        .daemon_addr
        .or_else(|| std::env::var("ANDLERD_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_DAEMON_ADDR.to_string());

    let mut client = AndlerServiceClient::connect(addr).await?;

    match cli.command {
        None => {
            wizard::handle_wizard(&mut client).await?;
        }
        Some(Command::Wizard {}) => {
            wizard::handle_wizard(&mut client).await?;
        }
        Some(Command::Create {
            file,
            kind,
            name,
            ovmf_vars_template,
            iso_path,
            disk_path,
            disk_size_gib,
            compact_on_shutdown,
            cdrom_bus,
            no_uefi,
            quick,
            dry_run,
            verify,
            android_version,
            base_image_path,
            gapps,
            microg,
            arm_translator,
            instances_root,
            overlay_size_gib,
        }) => {
            create::handle(
                &mut client, file, kind, name, ovmf_vars_template,
                iso_path, disk_path, disk_size_gib, compact_on_shutdown, cdrom_bus,
                no_uefi, quick, dry_run, verify,
                android_version, base_image_path, gapps, microg, arm_translator,
                instances_root, overlay_size_gib,
            ).await?;
        }
        Some(Command::Start(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_start(&mut client, id.to_string()).await?;
        }
        Some(Command::Stop(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_stop(&mut client, id.to_string(), args.graceful).await?;
        }
        Some(Command::Pause(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_pause(&mut client, id.to_string()).await?;
        }
        Some(Command::Resume(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_resume(&mut client, id.to_string()).await?;
        }
        Some(Command::Status(args)) => {
            let id = args.resolve_id()?;
            status::handle_status(&mut client, id.to_string()).await?;
        }
        Some(Command::List { full_id, state, name, sort, json }) => {
            status::handle_list(&mut client, full_id, state, name, sort, json).await?;
        }
        Some(Command::Remove(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_remove(&mut client, id.to_string(), args.purge).await?;
        }
        Some(Command::Config { action, flags }) => {
            match action {
                Some(ConfigCommand::View { instance_id }) => {
                    let id = instance_id
                        .as_deref()
                        .or(flags.instance.as_deref())
                        .ok_or("instance ID required")?;
                    status::handle_config(&mut client, id.to_string()).await?;
                }
                Some(ConfigCommand::Edit { instance_id }) => {
                    let id = instance_id
                        .as_deref()
                        .or(flags.instance.as_deref())
                        .ok_or("instance ID required")?;
                    edit::handle(&mut client, id.to_string()).await?;
                }
                Some(ConfigCommand::Set { instance_id, key, value }) => {
                    eprintln!(
                        "config set {instance_id} {key}={value} — not yet implemented"
                    );
                }
                None => {
                    // Flag form: config --instance <id> [--edit] [--file ...]
                    let id = flags
                        .instance
                        .as_deref()
                        .ok_or("instance ID required: use --instance or a subcommand")?;
                    if flags.edit {
                        edit::handle(&mut client, id.to_string()).await?;
                    } else if flags.file.is_some() {
                        eprintln!("config --file not yet implemented");
                    } else {
                        status::handle_config(&mut client, id.to_string()).await?;
                    }
                }
            }
        }
        Some(Command::Logs(args)) => {
            let id = args.resolve_id()?;
            status::handle_logs(
                &mut client,
                id.to_string(),
                args.source,
                args.grep,
                args.tail,
            )
            .await?;
        }
        Some(Command::Metrics(args)) => {
            let id = args.resolve_id()?;
            status::handle_metrics(&mut client, id.to_string(), args.once, args.json)
                .await?;
        }
        Some(Command::Clone {
            source_instance_id,
            name,
            instances_root,
            mode,
        }) => {
            clone::handle_clone(
                &mut client,
                source_instance_id,
                name,
                instances_root,
                mode,
            )
            .await?;
        }
        Some(Command::Export {
            source_instance_id,
            dest_path,
        }) => {
            clone::handle_export(&mut client, source_instance_id, dest_path).await?;
        }
        Some(Command::Snapshot { action }) => {
            // Extract instance_id from the action variant for the handler
            let instance_id = match &action {
                SnapshotAction::Create { instance_id, .. } => instance_id.clone(),
                SnapshotAction::Restore { instance_id, .. } => instance_id.clone(),
                SnapshotAction::Delete { instance_id, .. } => instance_id.clone(),
                SnapshotAction::List { instance_id } => instance_id.clone(),
            };
            snapshot::handle(&mut client, instance_id, action).await?;
        }
        Some(Command::Disk { action, flags }) => {
            let resolved = match action {
                Some(a) => a,
                None => {
                    // Resolve from flags
                    let path = flags
                        .path
                        .ok_or("disk: --path is required in flag form")?;
                    let path = std::path::PathBuf::from(path);
                    if flags.create {
                        let size = flags
                            .size
                            .ok_or("disk: --size is required for --create")?;
                        DiskAction::Create { path, size }
                    } else if flags.info {
                        DiskAction::Info { path }
                    } else if flags.resize {
                        let size = flags
                            .size
                            .ok_or("disk: --size is required for --resize")?;
                        DiskAction::Resize {
                            path,
                            size,
                            shrink: flags.shrink,
                        }
                    } else if flags.compact {
                        DiskAction::Compact { path }
                    } else {
                        return Err(
                            "disk: specify an action (--create, --info, --resize, --compact)"
                                .into(),
                        );
                    }
                }
            };
            disk::handle(resolved).await?;
        }
        Some(Command::Guest { action }) => {
            guest::handle(&mut client, action).await?;
        }
        // Always handled and returned from above, before connecting to
        // the daemon — see the early-return block above `let addr = ...`.
        // Listed here only because `match` must be exhaustive over
        // every `Command` variant.
        Some(Command::Completions { .. }) => unreachable!(),
    }

    Ok(())
}

#[cfg(test)]
mod main_error_formatting_tests {
    use super::*;

    #[test]
    fn format_grpc_error_not_found() {
        let status = tonic::Status::not_found("instance \"a1b2c3\" not found");
        assert_eq!(
            format_grpc_error(&status),
            "instance not found: instance \"a1b2c3\" not found"
        );
    }

    #[test]
    fn format_grpc_error_failed_precondition() {
        let status = tonic::Status::failed_precondition("cannot remove running instance");
        assert_eq!(
            format_grpc_error(&status),
            "cannot perform operation: cannot remove running instance"
        );
    }

    #[test]
    fn format_grpc_error_falls_back_for_unmapped_codes() {
        let status = tonic::Status::unauthenticated("no credentials");
        assert_eq!(format_grpc_error(&status), "error: no credentials");
    }

    #[derive(Debug)]
    struct WrappedError {
        source: std::io::Error,
    }

    impl std::fmt::Display for WrappedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "transport error")
        }
    }

    impl std::error::Error for WrappedError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.source)
        }
    }

    #[test]
    fn looks_like_daemon_not_running_matches_top_level_message() {
        let err = std::io::Error::new(std::io::ErrorKind::Other, "connection refused");
        assert!(looks_like_daemon_not_running(&err));
    }

    #[test]
    fn looks_like_daemon_not_running_walks_source_chain() {
        // Regression case: the top-level error's own message is a
        // generic "transport error" (matches too, but exercising the
        // chain-walk specifically here), while the actual OS-level
        // detail ("os error 111") only appears in `.source()`.
        let inner = std::io::Error::from_raw_os_error(111);
        let wrapped = WrappedError { source: inner };
        assert!(looks_like_daemon_not_running(&wrapped));
    }

    #[test]
    fn looks_like_daemon_not_running_is_false_for_unrelated_errors() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        assert!(!looks_like_daemon_not_running(&err));
    }
}
