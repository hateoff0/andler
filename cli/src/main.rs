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
    command: Option<Command>,
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
        /// Only show instances in this state (case-insensitive, e.g.
        /// `Running`, `stopped`).
        #[arg(long)]
        state: Option<String>,
        /// Only show instances whose name matches this regular
        /// expression.
        #[arg(long)]
        name: Option<String>,
        /// Sort order. Instance creation time isn't tracked by andlerd
        /// today, so "by created date" isn't an available sort key.
        #[arg(long, value_enum, default_value = "none")]
        sort: ListSortKey,
        /// Machine-readable JSON array output instead of one line per
        /// instance.
        #[arg(long)]
        json: bool,
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
    /// Edit instance configuration in $EDITOR/$VISUAL (falls back to
    /// `vi`) as TOML, then apply the changes. Does not restart a running
    /// instance — changes apply the next time it starts.
    Edit { instance_id: String },
    /// Stream stdout/stderr from the instance's hypervisor process.
    Logs {
        instance_id: String,
        /// Only show lines from this stream.
        #[arg(long, value_enum)]
        source: Option<CliLogSource>,
        /// Only show lines matching this regular expression.
        #[arg(long)]
        grep: Option<String>,
        /// Limit how much backlog is shown before continuing to follow
        /// live output, instead of the full history. Approximate, not
        /// exact: andlerd sends history and live lines as a single
        /// unbroken stream with no marker between them, so the CLI
        /// guesses where "history" ends by watching for a short pause
        /// in arriving lines (see `cli/src/status.rs::handle_logs`) —
        /// if history is still trickling in when that pause happens,
        /// more than N lines may be shown.
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Stream resource metrics (CPU%, RAM, disk I/O, net I/O, GPU) in real time.
    Metrics {
        instance_id: String,
        /// Print a single sample and exit, instead of streaming
        /// continuously.
        #[arg(long)]
        once: bool,
        /// Machine-readable JSON output (one object per sample).
        /// Combine with --once for a single JSON object; without it,
        /// prints one JSON object per line as samples arrive.
        #[arg(long)]
        json: bool,
    },
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
    /// Guest agent operations (install/remove packages in guest OS).
    ///
    /// Auto-fallback: if VM is running and guest agent is available → online
    /// via guest-exec; if VM is stopped → offline via qemu-nbd.
    Guest {
        /// Action: install, remove, or list.
        #[command(subcommand)]
        action: guest::GuestAction,
        /// Package name (e.g., "spice-vdagent"). Required for install/remove.
        package: Option<String>,
        /// Instance ID (full UUID or 8-char prefix).
        instance_id: String,
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
    /// Restore from a snapshot (requires Running/Paused).
    Restore {
        #[arg(long)]
        tag: String,
        /// Per-operation timeout in seconds. Overrides instance default (30s).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Delete a snapshot (requires Running/Paused).
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
    // completion script is a pure, local, offline operation (just
    // walks `Cli`'s own clap definition) — unlike every other
    // subcommand, it has no reason to require a running daemon, and
    // shouldn't fail with a connection error if one isn't running. See
    // PLAN.md, item 12, "Shell completions".
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
        }) => {
            create::handle(
                &mut client, file, kind, name, ovmf_vars_template,
                iso_path, disk_path, disk_size_gib, compact_on_shutdown, cdrom_bus,
                quick,
                android_version, base_image_path, gapps, microg, arm_translator, root,
                instances_root, overlay_size_gib, magisk_dir,
            ).await?;
        }
        Some(Command::Start { instance_id }) => {
            lifecycle::handle_start(&mut client, instance_id).await?;
        }
        Some(Command::Stop {
            instance_id,
            graceful,
        }) => {
            lifecycle::handle_stop(&mut client, instance_id, graceful).await?;
        }
        Some(Command::Pause { instance_id }) => {
            lifecycle::handle_pause(&mut client, instance_id).await?;
        }
        Some(Command::Resume { instance_id }) => {
            lifecycle::handle_resume(&mut client, instance_id).await?;
        }
        Some(Command::Status { instance_id }) => {
            status::handle_status(&mut client, instance_id).await?;
        }
        Some(Command::List { full_id, state, name, sort, json }) => {
            status::handle_list(&mut client, full_id, state, name, sort, json).await?;
        }
        Some(Command::Remove { instance_id, purge }) => {
            lifecycle::handle_remove(&mut client, instance_id, purge).await?;
        }
        Some(Command::Config { instance_id }) => {
            status::handle_config(&mut client, instance_id).await?;
        }
        Some(Command::Edit { instance_id }) => {
            edit::handle(&mut client, instance_id).await?;
        }
        Some(Command::Logs { instance_id, source, grep, tail }) => {
            status::handle_logs(&mut client, instance_id, source, grep, tail).await?;
        }
        Some(Command::Metrics { instance_id, once, json }) => {
            status::handle_metrics(&mut client, instance_id, once, json).await?;
        }
        Some(Command::Clone {
            source_instance_id,
            name,
            instances_root,
            mode,
        }) => {
            clone::handle_clone(&mut client, source_instance_id, name, instances_root, mode).await?;
        }
        Some(Command::Export {
            source_instance_id,
            dest_path,
        }) => {
            clone::handle_export(&mut client, source_instance_id, dest_path).await?;
        }
        Some(Command::Snapshot {
            instance_id,
            action,
        }) => {
            snapshot::handle(&mut client, instance_id, action).await?;
        }
        Some(Command::Disk { action }) => {
            disk::handle(action).await?;
        }
        Some(Command::Guest { action, package, instance_id }) => {
            guest::handle(&mut client, action, package, instance_id).await?;
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
