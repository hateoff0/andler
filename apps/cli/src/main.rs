mod cache;
mod clone;
mod connect;
mod create;
mod disk;
mod doctor;
mod edit;
mod events;
mod exec;
mod export_oci;
mod guest;
mod helpers;
mod hotplug;
mod image;
mod instance_file;
mod lifecycle;
mod op;
mod preview;
mod snapshot;
mod status;
mod verify;
mod wizard;

use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, ArmTranslator as ProtoArmTranslator, Empty,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use tonic::transport::Channel;

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
    #[arg(long, global = true)]
    daemon_addr: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// Whether `--json` was requested for the active subcommand: the single
    /// check for both the success output (handlers) and the unified error
    /// output in `main`.
    fn json_requested(&self) -> bool {
        self.command
            .as_ref()
            .map(|c| c.json_requested())
            .unwrap_or(false)
    }
}

macro_rules! dual_id_args {
    ($name:ident $(, $($extra:tt)*)?) => {
        #[derive(Args)]
        pub struct $name {

            pub instance_id: Option<String>,

            #[arg(long, short)]
            pub instance: Option<String>,

            $($($extra)*)?
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

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ConnectLevel {
    /// Pick the best available level automatically
    Auto,
    /// Serial console (works on any running VM)
    Console,
    /// Open an ssh session to the guest's forwarded port 22
    Ssh,
    /// Connect adb to the guest's forwarded port 5555
    Adb,
}

dual_id_args!(StartArgs,
    #[arg(long)]
    pub json: bool,
);
dual_id_args!(PauseArgs,
    #[arg(long)]
    pub json: bool,
);
dual_id_args!(ResumeArgs,
    #[arg(long)]
    pub json: bool,
);
dual_id_args!(StatusArgs,
    #[arg(long)]
    pub json: bool,
);
dual_id_args!(StopArgs,
    #[arg(long, help = "SIGTERM the guest OS (default: immediate SIGKILL)")]
    pub graceful: bool,

    #[arg(long)]
    pub json: bool,
);

dual_id_args!(RemoveArgs,
    #[arg(long, help = "Also delete the disk image and instance directory")]
    pub purge: bool,

    #[arg(long)]
    pub json: bool,
);

#[derive(Args)]
pub struct EventsArgs {
    /// Instance id filter (full or prefix); all instances when omitted
    instance_id: Option<String>,

    /// Keep streaming (default: exit after the first event)
    #[arg(long)]
    follow: bool,

    /// JSON-lines output
    #[arg(long)]
    json: bool,
}

dual_id_args!(LogsArgs,
    #[arg(long, value_enum)]
    pub(crate) source: Option<CliLogSource>,

    #[arg(long)]
    pub grep: Option<String>,

    #[arg(long)]
    pub tail: Option<usize>,

    /// Only for `andler logs daemon`: follow new lines.
    #[arg(long)]
    pub follow: bool,

    /// Only for `andler logs daemon`: JSON-lines output.
    #[arg(long)]
    pub json: bool,

    /// Only for `andler logs daemon`: only lines at/after this epoch-ms.
    #[arg(long)]
    pub since: Option<u64>,
);

dual_id_args!(MetricsArgs,
    #[arg(long)]
    pub once: bool,

    #[arg(long)]
    pub json: bool,
);

#[derive(Subcommand)]
pub enum ConfigCommand {
    View {
        instance_id: Option<String>,
    },

    Edit {
        instance_id: Option<String>,
        #[arg(
            long,
            help = "Emit the applied change (or lack of one) as JSON instead of \
                    human-readable text; the interactive edit itself is unaffected"
        )]
        json: bool,
    },

    Set {
        instance_id: String,

        key: String,

        value: String,
    },

    /// File-vs-memory diff: what `instance.toml` holds vs what the daemon
    /// loaded, plus the live-applied resolution when one is pending.
    Status {
        instance_id: Option<String>,
        #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
        json: bool,
    },
}

impl ConfigCommand {
    /// Whether `--json` was requested for a config subcommand. Only
    /// `config status` emits JSON.
    fn json_requested(&self) -> bool {
        match self {
            ConfigCommand::Status { json, .. } => *json,
            ConfigCommand::Edit { json, .. } => *json,
            _ => false,
        }
    }
}

#[derive(Args)]
pub struct ConfigFlags {
    #[arg(long, short)]
    pub instance: Option<String>,

    #[arg(long, short = 'e')]
    pub edit: bool,

    #[arg(long, short)]
    #[arg(long, short)]
    pub file: Option<PathBuf>,

    #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
    pub json: bool,
}

#[derive(Args)]
pub struct DiskFlags {
    #[arg(long, short = 'c')]
    pub create: bool,

    #[arg(long, short = 'i')]
    pub info: bool,

    #[arg(long, short = 'r')]
    pub resize: bool,

    #[arg(long, short = 'm')]
    pub compact: bool,

    #[arg(long, short = 'p')]
    pub path: Option<String>,

    #[arg(long, short = 's')]
    pub size: Option<String>,

    #[arg(long)]
    pub shrink: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new VM instance (from a TOML file, CLI flags, or the interactive wizard)
    Create {
        #[arg(long)]
        file: Option<PathBuf>,

        #[arg(long)]
        kind: Option<CliKind>,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        ovmf_vars_template: Option<String>,

        #[arg(long)]
        iso_path: Option<String>,

        #[arg(long)]
        disk_path: Option<String>,

        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        disk_size_gib: Option<u64>,

        #[arg(long)]
        compact_on_shutdown: bool,

        #[arg(long, value_enum, default_value = "auto")]
        cdrom_bus: CliCdromBus,

        #[arg(long)]
        no_uefi: bool,

        #[arg(long, help = "Skip prompts and use wizard defaults (requires --kind)")]
        quick: bool,

        #[arg(
            long,
            help = "Start from a template (built-ins: headless, desktop, android-gaming; \
                    or ~/.andler/templates/<name>.toml); CLI flags override it"
        )]
        template: Option<String>,

        #[arg(long, help = "Print the request that would be sent, without creating")]
        dry_run: bool,

        #[arg(long, help = "Validate the configuration and exit without creating")]
        verify: bool,

        #[arg(long, value_enum)]
        android_version: Option<CliAndroidVersion>,

        #[arg(long)]
        base_image_path: Option<String>,

        #[arg(long)]
        gapps: bool,

        #[arg(long)]
        microg: bool,

        #[arg(long, value_enum)]
        arm_translator: Option<CliArmTranslator>,

        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,

        #[arg(long, default_value_t = 128, value_parser = clap::value_parser!(u64).range(1..))]
        overlay_size_gib: u64,

        #[arg(long)]
        linked_overlay: bool,

        #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
        json: bool,
    },

    /// Connect to a guest: serial console, ssh, adb, or exec (auto-picks
    /// the best available level)
    Connect {
        instance_id: String,

        #[arg(long, value_enum, default_value = "auto")]
        level: ConnectLevel,

        #[arg(
            long,
            help = "Resolve and report the level/port as JSON instead of connecting"
        )]
        json: bool,
    },

    /// Run a command in the guest through the guest agent (exit code relayed)
    Exec {
        instance_id: String,
        /// Command and arguments to run in the guest
        #[arg(last = true, required = true, num_args = 1..)]
        argv: Vec<String>,

        /// Emit the result as JSON (exit_code, stdout, stderr) instead of
        /// printing stdout/stderr directly
        #[arg(long)]
        json: bool,
    },

    /// Start a stopped instance (boots QEMU)
    Start(StartArgs),

    /// Stop a running instance (SIGTERM, or SIGKILL after a grace period with --graceful)
    Stop(StopArgs),

    /// Pause a running instance (freezes the vCPU)
    Pause(PauseArgs),

    /// Resume a paused instance
    Resume(ResumeArgs),

    /// Show the current state of an instance
    Status(StatusArgs),

    /// List all instances
    List {
        #[arg(long = "full-id", short = 'q')]
        full_id: bool,

        #[arg(long)]
        state: Option<String>,

        #[arg(long)]
        name: Option<String>,

        #[arg(long, value_enum, default_value = "none")]
        sort: ListSortKey,

        #[arg(long)]
        json: bool,
    },

    /// Remove an instance (with --purge also deletes its disk image and instance directory)
    Remove(RemoveArgs),

    /// View or modify an instance's configuration
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
        #[command(flatten)]
        flags: ConfigFlags,
    },

    /// Stream an instance's QEMU stdout/stderr
    Logs(LogsArgs),

    /// Stream daemon events (lifecycle, operations, QMP); optional instance filter
    Events(EventsArgs),

    /// Stream per-instance metrics (CPU, memory, disk, network, GPU)
    Metrics(MetricsArgs),

    /// Clone an existing Android instance (linked, full-standalone, or shared-base)
    Clone {
        source_instance_id: String,

        #[arg(long)]
        name: String,

        #[arg(long, default_value_t = default_instances_root())]
        instances_root: String,

        #[arg(long, value_enum)]
        mode: CliCloneMode,

        /// Client-supplied idempotency key: a network retry with the same
        /// token joins the clone already running instead of creating a second
        /// instance.
        #[arg(long)]
        idempotency_token: Option<String>,
        #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
        json: bool,
    },

    /// Export an instance's disk image to a standalone file
    Export {
        source_instance_id: String,
        dest_path: String,

        /// Client-supplied idempotency key: a network retry with the same
        /// token joins the export already running to the same destination.
        #[arg(long)]
        idempotency_token: Option<String>,

        #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
        json: bool,
    },

    /// Export an instance's disk as an OCI image layout
    ExportOci {
        source_instance_id: String,

        dest_path: String,

        #[arg(long, value_enum)]
        disk_format: CliDiskFormat,

        #[arg(long)]
        disk_path: Option<String>,

        #[arg(long, help = "Emit the result as JSON instead of human-readable text")]
        json: bool,
    },

    /// Manage QEMU snapshots (create, restore, delete, list)
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,

        #[arg(long)]
        json: bool,
    },

    /// Inspect and cancel long-running operations
    Op {
        #[command(subcommand)]
        action: OpAction,

        #[arg(long)]
        json: bool,
    },

    /// Create, inspect, resize, or compact disk images
    Disk {
        #[command(subcommand)]
        action: Option<DiskAction>,
        #[command(flatten)]
        flags: DiskFlags,
        #[arg(
            long,
            global = true,
            help = "Emit the result as JSON instead of human-readable text"
        )]
        json: bool,
    },

    /// Hot-plug a disk or network device into a running instance
    Attach {
        #[command(subcommand)]
        action: AttachAction,

        /// Emit the result as JSON instead of the human-readable message
        #[arg(long)]
        json: bool,
    },

    /// Hot-unplug a disk or network device from a running instance
    Detach {
        #[command(subcommand)]
        action: DetachAction,

        /// Emit the result as JSON instead of the human-readable message
        #[arg(long)]
        json: bool,
    },

    /// Manage guest packages and Android boot mode (requires the andlerd daemon)
    Guest {
        #[command(subcommand)]
        action: guest::GuestAction,

        #[arg(
            long,
            global = true,
            help = "Emit the result as JSON instead of human-readable text"
        )]
        json: bool,
    },

    /// Interactive configuration wizard (hardware auto-detection)
    Wizard {},

    /// Check the host environment for andler prerequisites
    Doctor {
        /// Print the daemon's internal metrics snapshot: RPC
        /// latency p50/p99 by method, error counts by status code,
        /// instance/active-op counts, QMP reconnect count.
        #[arg(long)]
        metrics: bool,

        /// Emit the check results as JSON instead of the human-readable report
        #[arg(long)]
        json: bool,
    },

    /// Manage the base-image cache (list builds, clean superseded ones)
    Cache {
        #[command(subcommand)]
        action: cache::CacheAction,

        #[arg(
            long,
            global = true,
            help = "Emit the result as JSON instead of human-readable text"
        )]
        json: bool,
    },

    /// Base images published by the release pipeline: list and download them
    Image {
        #[command(subcommand)]
        action: image::ImageAction,

        #[arg(
            long,
            global = true,
            help = "Emit the result as JSON instead of human-readable text"
        )]
        json: bool,
    },

    /// Generate shell completion scripts
    Completions { shell: clap_complete::Shell },
}

impl Command {
    /// Whether `--json` was requested for this subcommand: the single check
    /// for both the success output (handlers) and the unified error output
    /// in `main`.
    fn json_requested(&self) -> bool {
        match self {
            // Struct variants: `json` is a `&bool`.
            Command::Create { json, .. }
            | Command::List { json, .. }
            | Command::Clone { json, .. }
            | Command::Export { json, .. }
            | Command::Snapshot { json, .. }
            | Command::ExportOci { json, .. }
            | Command::Op { json, .. }
            | Command::Disk { json, .. }
            | Command::Guest { json, .. }
            | Command::Cache { json, .. }
            | Command::Image { json, .. }
            | Command::Exec { json, .. }
            | Command::Doctor { json, .. }
            | Command::Attach { json, .. }
            | Command::Detach { json, .. } => *json,
            // Tuple variants: each has a distinct args type; read its `json` field.
            Command::Status(args) => args.json,
            Command::Logs(args) => args.json,
            Command::Events(args) => args.json,
            Command::Metrics(args) => args.json,
            Command::Config { action, flags } => action
                .as_ref()
                .map(|a| a.json_requested())
                .unwrap_or(flags.json),
            Command::Connect { json, .. } => *json,
            Command::Start(_)
            | Command::Stop(_)
            | Command::Pause(_)
            | Command::Resume(_)
            | Command::Remove(_)
            | Command::Wizard {}
            | Command::Completions { .. } => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliLogSource {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum ListSortKey {
    #[default]
    None,
    Name,
    State,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliKind {
    Linux,

    Android,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum CliCdromBus {
    #[default]
    Auto,

    Virtio,

    Ide,
}

#[derive(Subcommand)]
enum SnapshotAction {
    Create {
        instance_id: String,

        #[arg(long)]
        tag: String,

        #[arg(long)]
        description: Option<String>,

        #[arg(long)]
        timeout: Option<u64>,
    },

    Restore {
        instance_id: String,
        #[arg(long)]
        tag: String,

        #[arg(long)]
        timeout: Option<u64>,

        /// Keep the current disk chain as an archived branch instead of
        /// discarding layers newer than the target.
        #[arg(long)]
        branch: bool,

        /// Client-supplied idempotency key: a network retry with the same
        /// token joins the in-flight restore instead of starting a second one.
        #[arg(long)]
        idempotency_token: Option<String>,
    },

    Delete {
        instance_id: String,
        #[arg(long)]
        tag: String,

        #[arg(long)]
        timeout: Option<u64>,
    },

    List {
        instance_id: String,
    },
}

#[derive(Subcommand)]
enum OpAction {
    /// List active operations across all instances
    List {},

    /// Request cooperative cancellation of a running operation
    Cancel {
        /// Operation id from `andler op list`
        op_id: String,
    },
}

#[derive(Subcommand)]
enum DiskAction {
    Create {
        path: PathBuf,

        #[arg(long)]
        size: String,
    },

    Info {
        path: PathBuf,
    },

    Resize {
        path: PathBuf,

        #[arg(long)]
        size: String,

        #[arg(long)]
        shrink: bool,
    },

    Compact {
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum AttachAction {
    /// Attach a disk image; creates a new qcow2 when the path does not exist (--size required then)
    Disk {
        /// Instance ID or unique prefix
        instance_id: String,

        /// Path of the disk image; defaults to disk-extraN.qcow2 inside the instance directory
        #[arg(long)]
        path: Option<PathBuf>,

        /// Virtual size of a new disk (e.g. 20G); ignored for existing images
        #[arg(long, value_parser = helpers::parse_size)]
        size: Option<u64>,
    },

    /// Attach a network device
    Net {
        /// Instance ID or unique prefix
        instance_id: String,

        #[arg(long, value_enum, default_value = "nat")]
        mode: CliAttachNetMode,

        /// Bridge interface to use with --mode bridge
        #[arg(long)]
        bridge: Option<String>,

        #[arg(long, default_value = "virtio-net-pci")]
        model: String,

        #[arg(long, value_enum, default_value = "slirp")]
        nat_backend: CliNatBackend,
    },
}

#[derive(Subcommand)]
enum DetachAction {
    /// Detach a disk image by path (the file itself is kept)
    Disk {
        /// Instance ID or unique prefix
        instance_id: String,

        /// Path of the attached disk, as shown by `andler config <id>`
        path: PathBuf,
    },

    /// Detach a network device by its index (0-based, in attach order)
    Net {
        /// Instance ID or unique prefix
        instance_id: String,

        /// Index of the extra network device, as shown by `andler config <id>`
        index: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliAttachNetMode {
    Nat,
    Bridge,
    Isolated,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum CliNatBackend {
    #[default]
    Slirp,
    Passt,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliCloneMode {
    Linked,

    #[value(name = "full-standalone")]
    FullStandalone,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliDiskFormat {
    Qcow2,

    Raw,

    Vdi,
}

impl From<CliDiskFormat> for andler_rpc::proto::DiskFormat {
    fn from(value: CliDiskFormat) -> Self {
        match value {
            CliDiskFormat::Qcow2 => andler_rpc::proto::DiskFormat::Qcow2,
            CliDiskFormat::Raw => andler_rpc::proto::DiskFormat::Raw,
            CliDiskFormat::Vdi => andler_rpc::proto::DiskFormat::Vdi,
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

impl CliAndroidVersion {
    /// The bare version number, for prose ("Android 13", not "Android
    /// Android13" — the enum's Debug name is not a word).
    pub(crate) fn number(&self) -> &'static str {
        match self {
            CliAndroidVersion::Android11 => "11",
            CliAndroidVersion::Android13 => "13",
        }
    }
}

impl From<CliAndroidVersion> for ProtoAndroidVersion {
    fn from(value: CliAndroidVersion) -> Self {
        match value {
            CliAndroidVersion::Android11 => ProtoAndroidVersion::Android11,
            CliAndroidVersion::Android13 => ProtoAndroidVersion::Android13,
        }
    }
}

impl From<CliAndroidVersion> for andler_core::AndroidVersion {
    fn from(value: CliAndroidVersion) -> Self {
        match value {
            CliAndroidVersion::Android11 => andler_core::AndroidVersion::Android11,
            CliAndroidVersion::Android13 => andler_core::AndroidVersion::Android13,
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

impl From<CliArmTranslator> for andler_core::ArmTranslator {
    fn from(value: CliArmTranslator) -> Self {
        match value {
            CliArmTranslator::None => andler_core::ArmTranslator::None,
            CliArmTranslator::Libndk => andler_core::ArmTranslator::Libndk,
            CliArmTranslator::Libhoudini => andler_core::ArmTranslator::Libhoudini,
        }
    }
}

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

/// Queries the daemon's version and fails with an actionable message when
/// it differs from this CLI.s build. Stale daemons from an earlier
/// refactor phase produce opaque protobuf errors; name the mismatch instead.
/// Stamps every request with a `request_id` metadata header: the
/// daemon's RPC span carries it, so a single log line reconstructs the
/// path CLI → RPC → operation.
#[derive(Clone)]
pub(crate) struct RequestIdInterceptor;

impl tonic::service::Interceptor for RequestIdInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let id = format!("{:032x}", rand::random::<u128>());
        if let Ok(value) = id.parse() {
            request.metadata_mut().insert("request_id", value);
        }
        Ok(request)
    }
}

pub(crate) type TracedClient =
    AndlerServiceClient<tonic::codegen::InterceptedService<Channel, RequestIdInterceptor>>;

/// Connects a client that stamps every request with a `request_id`
/// metadata header: the daemon's RPC span carries it, so a single
/// log line reconstructs the path CLI → RPC → operation.
pub(crate) async fn traced_client(addr: &str) -> Result<TracedClient, Box<dyn std::error::Error>> {
    Ok(AndlerServiceClient::with_interceptor(
        Channel::from_shared(addr.to_string())?.connect().await?,
        RequestIdInterceptor,
    ))
}

async fn check_daemon_version(client: &mut TracedClient) -> Result<(), String> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let daemon_version = client
        .get_version(Empty {})
        .await
        .map_err(|e| format!("cannot query daemon version: {e}"))?
        .into_inner()
        .version;
    if daemon_version == cli_version {
        Ok(())
    } else {
        Err(format!(
            "daemon is version {daemon_version}, this CLI was built for {cli_version}; \
             restart the daemon (`systemctl --user restart andlerd` or kill andlerd) so \
             both sides speak the same protocol"
        ))
    }
}

/// Resolves the daemon address from `--daemon-addr`, then `ANDLERD_ADDR`,
/// then the default. Shared by `main` and `run` so the printed error and the
/// connection attempt agree on the address.
fn resolve_daemon_addr(cli: &Cli) -> String {
    cli.daemon_addr
        .clone()
        .or_else(|| std::env::var("ANDLERD_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_DAEMON_ADDR.to_string())
}

/// Message for a CLI error, reused by both the text and JSON output paths so
/// they never diverge. Mirrors the previous inline rendering exactly.
fn cli_error_message(err: &(dyn std::error::Error + 'static), addr: &str) -> String {
    if let Some(status) = err.downcast_ref::<tonic::Status>() {
        format_grpc_error(status)
    } else if looks_like_daemon_not_running(err) {
        format!("andlerd is not running at {addr}.\nStart it with: andlerd")
    } else {
        err.to_string()
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let addr = resolve_daemon_addr(&cli);
    let json = cli.json_requested();

    if let Err(err) = run(cli, &addr).await {
        // `--json` errors carry the same actionable message as the text
        // path, wrapped in a stable {"error": ...} shape.
        if json {
            let message = cli_error_message(err.as_ref(), &addr);
            let payload = serde_json::json!({ "error": message });
            eprintln!(
                "{}",
                serde_json::to_string(&payload).unwrap_or_else(|_| r#"{"error":""}"#.into())
            );
        } else {
            eprintln!("{}", cli_error_message(err.as_ref(), &addr));
        }
        return std::process::ExitCode::FAILURE;
    }

    std::process::ExitCode::SUCCESS
}

async fn run(cli: Cli, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(Command::Completions { shell }) = &cli.command {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(*shell, &mut cmd, bin_name, &mut std::io::stdout());
        return Ok(());
    }

    if let Some(Command::Doctor { metrics, json }) = &cli.command {
        if *metrics {
            return doctor::print_metrics(addr).await;
        }
        let all_ok = doctor::run(addr, *json).await;
        return if all_ok {
            Ok(())
        } else {
            Err("some checks failed".into())
        };
    }

    if let Some(Command::Cache { action, json }) = &cli.command {
        return cache::handle(action.clone(), *json);
    }

    let mut client = traced_client(addr).await?;

    // Version handshake: a CLI and daemon built from different
    // refactor phases would otherwise surface as an opaque protobuf error
    // mid-RPC. Report the mismatch explicitly and point at the fix.
    if let Err(msg) = check_daemon_version(&mut client).await {
        eprintln!("{msg}");
        return Err("incompatible daemon version".into());
    }

    match cli.command {
        None => {
            wizard::handle_wizard(&mut client, wizard::PartialArgs::default()).await?;
        }
        Some(Command::Wizard {}) => {
            wizard::handle_wizard(&mut client, wizard::PartialArgs::default()).await?;
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
            template,
            dry_run,
            verify,
            android_version,
            base_image_path,
            gapps,
            microg,
            arm_translator,
            instances_root,
            overlay_size_gib,
            linked_overlay,
            json,
        }) => {
            create::handle(
                &mut client,
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
                template,
                dry_run,
                verify,
                android_version,
                base_image_path,
                gapps,
                microg,
                arm_translator,
                instances_root,
                overlay_size_gib,
                linked_overlay,
                json,
            )
            .await?;
        }
        Some(Command::Connect {
            instance_id,
            level,
            json,
        }) => {
            connect::handle_connect(&mut client, instance_id, level, json).await?;
        }
        Some(Command::Exec {
            instance_id,
            argv,
            json,
        }) => {
            exec::handle_exec(&mut client, instance_id, argv, json).await?;
        }
        Some(Command::Start(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_start(&mut client, id.to_string(), args.json).await?;
        }
        Some(Command::Stop(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_stop(&mut client, id.to_string(), args.graceful, args.json).await?;
        }
        Some(Command::Pause(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_pause(&mut client, id.to_string(), args.json).await?;
        }
        Some(Command::Resume(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_resume(&mut client, id.to_string(), args.json).await?;
        }
        Some(Command::Status(args)) => {
            let id = args.resolve_id()?;
            status::handle_status(&mut client, id.to_string(), args.json).await?;
        }
        Some(Command::List {
            full_id,
            state,
            name,
            sort,
            json,
        }) => {
            status::handle_list(&mut client, full_id, state, name, sort, json).await?;
        }
        Some(Command::Remove(args)) => {
            let id = args.resolve_id()?;
            lifecycle::handle_remove(&mut client, id.to_string(), args.purge, args.json).await?;
        }
        Some(Command::Config { action, flags }) => match action {
            Some(ConfigCommand::View { instance_id }) => {
                let id = instance_id
                    .as_deref()
                    .or(flags.instance.as_deref())
                    .ok_or("instance ID required")?;
                status::handle_config(&mut client, id.to_string()).await?;
            }
            Some(ConfigCommand::Edit { instance_id, json }) => {
                let id = instance_id
                    .as_deref()
                    .or(flags.instance.as_deref())
                    .ok_or("instance ID required")?;
                edit::handle(&mut client, id.to_string(), json).await?;
            }
            Some(ConfigCommand::Status { instance_id, json }) => {
                let id = instance_id
                    .as_deref()
                    .or(flags.instance.as_deref())
                    .ok_or("instance ID required")?;
                status::handle_config_status(&mut client, id.to_string(), json).await?;
            }
            Some(ConfigCommand::Set {
                instance_id,
                key,
                value,
            }) => {
                client
                    .set_instance_config(andler_rpc::proto::SetInstanceConfigRequest {
                        instance_ref: instance_id.clone(),
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .await?;
                println!("{instance_id}: {key} = {value}");
            }
            None => {
                let id = flags
                    .instance
                    .as_deref()
                    .ok_or("instance ID required: use --instance or a subcommand")?;
                if flags.edit {
                    edit::handle(&mut client, id.to_string(), flags.json).await?;
                } else if flags.file.is_some() {
                    eprintln!("config --file not yet implemented");
                } else {
                    status::handle_config(&mut client, id.to_string()).await?;
                }
            }
        },
        Some(Command::Events(args)) => {
            crate::events::handle(&mut client, args.instance_id, args.follow, args.json).await?;
        }
        Some(Command::Logs(args)) => {
            let is_daemon = args.instance_id.as_deref() == Some("daemon")
                || args.instance.as_deref() == Some("daemon");
            if is_daemon {
                status::handle_daemon_logs(&mut client, args.follow, args.json, args.since).await?;
            } else {
                if args.follow || args.json || args.since.is_some() {
                    eprintln!(
                        "--follow/--json/--since apply to `andler logs daemon` only; \
                         instance logs stream everything"
                    );
                    std::process::exit(2);
                }
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
        }
        Some(Command::Metrics(args)) => {
            let id = args.resolve_id()?;
            status::handle_metrics(&mut client, id.to_string(), args.once, args.json).await?;
        }
        Some(Command::Clone {
            source_instance_id,
            name,
            instances_root,
            mode,
            idempotency_token,
            json,
        }) => {
            clone::handle_clone(
                &mut client,
                source_instance_id,
                name,
                instances_root,
                mode,
                idempotency_token,
                json,
            )
            .await?;
        }
        Some(Command::Export {
            source_instance_id,
            dest_path,
            idempotency_token,
            json,
        }) => {
            clone::handle_export(
                &mut client,
                source_instance_id,
                dest_path,
                idempotency_token,
                json,
            )
            .await?;
        }

        Some(Command::ExportOci {
            source_instance_id,
            dest_path,
            disk_format,
            disk_path,
            json,
        }) => {
            export_oci::handle_export_oci(
                &mut client,
                source_instance_id,
                dest_path,
                disk_format,
                disk_path,
                json,
            )
            .await?;
        }
        Some(Command::Snapshot { action, json }) => {
            let instance_id = match &action {
                SnapshotAction::Create { instance_id, .. } => instance_id.clone(),
                SnapshotAction::Restore { instance_id, .. } => instance_id.clone(),
                SnapshotAction::Delete { instance_id, .. } => instance_id.clone(),
                SnapshotAction::List { instance_id } => instance_id.clone(),
            };
            snapshot::handle(&mut client, instance_id, action, json).await?;
        }
        Some(Command::Op { action, json }) => {
            crate::op::handle(&mut client, action, json).await?;
        }
        Some(Command::Disk {
            action,
            flags,
            json,
        }) => {
            let resolved = match action {
                Some(a) => a,
                None => {
                    let path = flags.path.ok_or("disk: --path is required in flag form")?;
                    let path = std::path::PathBuf::from(path);

                    let chosen: Vec<&str> = [
                        (flags.create, "--create"),
                        (flags.info, "--info"),
                        (flags.resize, "--resize"),
                        (flags.compact, "--compact"),
                    ]
                    .iter()
                    .filter(|(picked, _)| *picked)
                    .map(|(_, name)| *name)
                    .collect();

                    match chosen.len() {
                        0 => {
                            return Err(
                                "disk: specify an action (--create, --info, --resize, --compact)"
                                    .into(),
                            );
                        }
                        1 => {}
                        _ => {
                            return Err(format!(
                                "disk: actions are mutually exclusive, got {}",
                                chosen.join(" and ")
                            )
                            .into());
                        }
                    }

                    if flags.create {
                        let size = flags.size.ok_or("disk: --size is required for --create")?;
                        DiskAction::Create { path, size }
                    } else if flags.info {
                        DiskAction::Info { path }
                    } else if flags.resize {
                        let size = flags.size.ok_or("disk: --size is required for --resize")?;
                        DiskAction::Resize {
                            path,
                            size,
                            shrink: flags.shrink,
                        }
                    } else {
                        DiskAction::Compact { path }
                    }
                }
            };
            disk::handle(resolved, json).await?;
        }
        Some(Command::Guest { action, json }) => {
            guest::handle(&mut client, action, json).await?;
        }
        Some(Command::Attach { action, json }) => {
            hotplug::handle_attach(&mut client, action, json).await?;
        }
        Some(Command::Detach { action, json }) => {
            hotplug::handle_detach(&mut client, action, json).await?;
        }
        Some(Command::Cache { .. }) => unreachable!(),
        Some(Command::Image { action, json }) => image::handle(&mut client, action, json).await?,
        Some(Command::Doctor { .. }) => unreachable!(),
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
