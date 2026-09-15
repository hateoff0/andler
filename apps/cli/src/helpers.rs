use andler_rpc::proto::{
    BackendKind, BaseImageDownloadPhase, BaseImageDownloadProgress, InstanceStateKind,
};
use std::future::Future;
use std::io::IsTerminal;

/// Searches $PATH, then common sbin directories that are often missing from a
/// regular (non-root) user's PATH but are exactly where `modprobe` and friends
/// usually live. Returns the first match's full path.
pub fn which(bin: &str) -> Option<std::path::PathBuf> {
    let from_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(bin);
            if full.is_file() {
                Some(full)
            } else {
                None
            }
        })
    });

    from_path.or_else(|| {
        ["/usr/sbin", "/sbin", "/usr/local/sbin"]
            .iter()
            .map(|dir| std::path::Path::new(dir).join(bin))
            .find(|full| full.is_file())
    })
}

pub fn state_kind_name(kind: InstanceStateKind) -> &'static str {
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

pub fn colorize_status(kind: InstanceStateKind, is_tty: bool) -> String {
    let name = state_kind_name(kind);
    if !is_tty {
        return name.to_string();
    }
    let code = match kind {
        InstanceStateKind::Running => "32", // green
        InstanceStateKind::Stopped | InstanceStateKind::Created => "2", // dim
        InstanceStateKind::Error => "31",   // red
        InstanceStateKind::Paused => "33",  // yellow
        InstanceStateKind::Starting | InstanceStateKind::Stopping => "36", // cyan
        InstanceStateKind::InstanceStateUnspecified => return name.to_string(),
    };
    format!("\x1b[{code}m{name}\x1b[0m")
}

pub fn backend_kind_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Unspecified => "UNSPECIFIED",
        BackendKind::Qemu => "Qemu",
    }
}

pub fn format_bytes(bytes: u64) -> String {
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

pub fn format_bytes_per_sec(bps: u64) -> String {
    format!("{}/s", format_bytes(bps))
}

pub fn parse_size(input: &str) -> Result<u64, String> {
    let input = input.trim().to_uppercase().replace(' ', "");
    if input.is_empty() {
        return Err("empty size string".to_string());
    }

    let split = input
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(input.len());

    let number_part = &input[..split];
    let unit_part = &input[split..];

    if number_part.is_empty() {
        return Err(format!("missing number before `{unit_part}`"));
    }
    if unit_part.starts_with('.') {
        return Err("decimal sizes not supported (use e.g. 64GB, not 1.5GB)".to_string());
    }

    let number: u64 = number_part
        .parse()
        .map_err(|e| format!("invalid number `{number_part}`: {e}"))?;

    let bytes = match unit_part {
        "" => number,
        "B" => number,
        "KB" | "KIB" | "K" => number
            .checked_mul(1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "MB" | "MIB" | "M" => number
            .checked_mul(1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "GB" | "GIB" | "G" => number
            .checked_mul(1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        "TB" | "TIB" | "T" => number
            .checked_mul(1024 * 1024 * 1024 * 1024)
            .ok_or_else(|| format!("size too large: {input}"))?,
        _ => {
            return Err(format!(
                "unknown unit `{unit_part}` (use B, KB/KiB, MB/MiB, GB/GiB, TB/TiB)"
            ))
        }
    };

    Ok(bytes)
}

pub fn ensure_qcow2_extension(path: &std::path::Path) -> std::path::PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension("qcow2")
    }
}

pub fn format_size(bytes: u64) -> String {
    const TIB: u64 = 1024 * 1024 * 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;

    if bytes > 0 && bytes.is_multiple_of(TIB) {
        format!("{} TiB", bytes / TIB)
    } else if bytes > 0 && bytes.is_multiple_of(GIB) {
        format!("{} GiB", bytes / GIB)
    } else if bytes > 0 && bytes.is_multiple_of(MIB) {
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

/// Formats an RFC3339 timestamp for display as local `2024-01-15 10:30:00`.
/// Falls back to the raw string when it isn't parseable (e.g. older data).
pub fn format_timestamp(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Docker-style short display ID: first 12 hex chars of the 64-char full ID.
pub fn short_id(full: &str) -> &str {
    full.get(..12).unwrap_or(full)
}

pub fn spinner(message: &str) -> indicatif::ProgressBar {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::time::Duration;

    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    if let Ok(style) = ProgressStyle::default_spinner().template("{spinner} {msg}") {
        pb.set_style(style);
    }
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

pub fn emit_json<T: serde::Serialize>(value: &T) -> std::result::Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// The progress bar a base-image download reports through, shared by
/// `andler image download` and the wizard's guest-image question so both show
/// the same thing for the same stream.
///
/// The line answers "how fast, and how long is this going to take" — a 2.3 GB
/// image takes minutes, and "272 MiB / 2.27 GiB" alone does not say whether
/// that is two minutes or forty. cliclack's stock download template spends 98
/// columns on it (symbol, message, 30-column bar, counters, ETA), and a live
/// frame wider than the terminal wraps, which leaves the wrapped remainder on
/// screen at the next redraw; so the shape is chosen for the terminal it is
/// drawn on, dropping the total and then the bar as the width shrinks.
/// Nothing is drawn when stderr is not a terminal, which is what leaves
/// non-interactive callers with their line-oriented output.
pub fn download_bar() -> cliclack::ProgressBar {
    let columns = terminal_columns(libc::STDERR_FILENO).unwrap_or(80);
    let template = if columns >= 110 {
        "{msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
    } else if columns >= 78 {
        "{msg} [{bar:10.cyan/blue}] {bytes} ({bytes_per_sec}, {eta})"
    } else {
        "{msg} ({bytes_per_sec})"
    };
    cliclack::progress_bar(1).with_template(template)
}

/// The terminal's width, for callers that size their output to it.
fn terminal_columns(fd: i32) -> Option<usize> {
    // SAFETY: TIOCGWINSZ only writes the size into the winsize this call owns.
    let (measured, size) = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        let measured = libc::ioctl(fd, libc::TIOCGWINSZ, &mut size);
        (measured, size)
    };
    (measured == 0 && size.ws_col > 0).then_some(size.ws_col as usize)
}

/// What the bar says for one stream message: deliberately short, because it
/// shares the line with the bar, the byte counters and the ETA, and the asset
/// names (`linux-waydroid-<id>.qcow2.zst.NN.part`) are far longer than that
/// budget. An empty line means "nothing to report" — the caller keeps what is
/// already on screen. The batch position appears only when the daemon knows
/// it: mid-transfer messages carry no index, and `(0/0)` is not worth showing.
pub fn download_bar_message(message: &BaseImageDownloadProgress) -> String {
    match message.phase() {
        BaseImageDownloadPhase::Downloading if message.asset_count > 0 => {
            format!(
                "downloading part {}/{}",
                message.asset_index, message.asset_count
            )
        }
        BaseImageDownloadPhase::Downloading => "downloading".to_string(),
        BaseImageDownloadPhase::Verifying => "verifying".to_string(),
        BaseImageDownloadPhase::Extracting => "unpacking the image".to_string(),
        BaseImageDownloadPhase::Installing => "installing the image".to_string(),
        BaseImageDownloadPhase::Resolving => "resolving the build".to_string(),
        BaseImageDownloadPhase::Done => "done".to_string(),
        BaseImageDownloadPhase::Unspecified => String::new(),
    }
}

/// Feeds one stream message into the bar: the position whenever the daemon
/// reports bytes (so the bar also advances through the extraction, which
/// reports the unpacked total), and the phase line whenever there is one.
pub fn update_download_bar(progress: &cliclack::ProgressBar, message: &BaseImageDownloadProgress) {
    if message.total_bytes > 0 {
        progress.set_length(message.total_bytes);
        progress.set_position(message.downloaded_bytes.min(message.total_bytes));
    }
    let line = download_bar_message(message);
    if !line.is_empty() {
        progress.set_message(line);
    }
}

/// Emit a value as compact JSON on stdout: the single source of truth for
/// `--json` output so every subcommand formats identically. Streaming outputs
/// (metrics, events) call this per sample to stay line-based.
/// Indeterminate progress for one blocking step (create, snapshot, …).
/// Hidden when stderr is not a terminal, so piped output stays line-oriented.
/// Runs a daemon call that can spend minutes inside a daemon-side operation
/// (translator switch, package install, profile apply) while reporting that
/// operation's phase.
///
/// The RPC is unary — it answers only when the work is done — so progress is
/// polled on a second client. `report` fires whenever the phase or the
/// percentage moves, with the elapsed time appended: `guest install libndk`
/// used to print one line and then sit silent while a libguestfs session
/// started, ~18 MiB came down and three appliance batches ran.
pub async fn call_with_operation_progress<T, F>(
    client: &crate::TracedClient,
    instance_ref: &str,
    label: &str,
    mut report: impl FnMut(&str),
    call: F,
) -> Result<T, tonic::Status>
where
    F: Future<Output = Result<T, tonic::Status>>,
{
    let mut poll = client.clone();
    tokio::pin!(call);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
    let started = std::time::Instant::now();
    let mut last_line = String::new();
    let mut last_report = std::time::Instant::now();
    // Snapshot what is already running *before* the call is polled (a future
    // does nothing until it is awaited, so the request has not been sent yet):
    // the operation this command starts must be recognisable as new, while one
    // another caller started must not be mistaken for ours.
    let mut watch = OperationWatch::default();
    let _ = running_operation(&mut poll, instance_ref, &mut watch).await;

    loop {
        tokio::select! {
            result = &mut call => return result,
            _ = ticker.tick() => {
                let line = match running_operation(&mut poll, instance_ref, &mut watch).await {
                    Ok(Some(line)) => line,
                    // Not every slow step is a daemon-side operation (an
                    // offline `guest list` mounts the disk inline, for
                    // example): name it, because silence is what made these
                    // commands look hung.
                    _ => label.to_string(),
                };

                // Report on every change, and otherwise every two seconds:
                // the line carries the elapsed time, so the heartbeat is what
                // tells the reader the operation is still moving. Without it a
                // phase the daemon has not subdivided (a slow download, a
                // libguestfs session) shows one frozen line for minutes.
                if line != last_line || last_report.elapsed() >= std::time::Duration::from_secs(2) {
                    last_line = line.clone();
                    last_report = std::time::Instant::now();
                    report(&format!("{line} — {}s", started.elapsed().as_secs()));
                }
            }
        }
    }
}

/// What this command is reporting on.
///
/// An operation that was already running when the command started belongs to
/// somebody else — a fast `guest list` used to display the progress of a
/// translator switch another caller had started — so the first poll records
/// what was already there and only a later appearance counts as ours.
#[derive(Default)]
struct OperationWatch {
    primed: bool,
    preexisting: Vec<String>,
    ours: Option<String>,
}

async fn running_operation(
    client: &mut crate::TracedClient,
    instance_ref: &str,
    watch: &mut OperationWatch,
) -> Result<Option<String>, tonic::Status> {
    use andler_rpc::proto::Empty;

    let operations = client
        .list_operations(Empty {})
        .await?
        .into_inner()
        .operations;
    // The caller may hold a prefix of the id (`andler guest install libndk 7a`),
    // so match either direction.
    let belongs = |operation: &andler_rpc::proto::OperationInfo| {
        operation.instance_id.starts_with(instance_ref)
            || (!operation.instance_id.is_empty()
                && instance_ref.starts_with(&operation.instance_id))
    };

    if !watch.primed {
        watch.preexisting = operations
            .iter()
            .map(|operation| operation.op_id.clone())
            .collect();
        watch.primed = true;
    }

    if watch.ours.is_none() {
        watch.ours = operations
            .iter()
            .find(|operation| belongs(operation) && !watch.preexisting.contains(&operation.op_id))
            .map(|operation| operation.op_id.clone());
    }

    let Some(ours) = watch.ours.as_deref() else {
        return Ok(None);
    };

    Ok(operations
        .iter()
        .find(|operation| operation.op_id == ours && belongs(operation))
        .map(render_operation))
}

fn render_operation(operation: &andler_rpc::proto::OperationInfo) -> String {
    format!(
        "{} — {} ({}%)",
        operation.kind,
        operation_phase(operation),
        (operation.progress * 100.0).round() as u32
    )
}

/// The daemon names the phase it is running. A daemon predating the field
/// publishes only the phase list and the overall progress, so fall back to the
/// first phase whose cumulative weight covers that progress.
fn operation_phase(op: &andler_rpc::proto::OperationInfo) -> String {
    if !op.current_phase.is_empty() {
        return op.current_phase.clone();
    }

    let total: f64 = op.phases.iter().map(|phase| phase.weight).sum();
    if op.phases.is_empty() || total <= 0.0 {
        return "working".to_string();
    }

    let target = op.progress * total;
    let mut accumulated = 0.0;
    for phase in &op.phases {
        accumulated += phase.weight;
        if target <= accumulated {
            return phase.name.clone();
        }
    }

    op.phases
        .last()
        .map(|phase| phase.name.clone())
        .unwrap_or_else(|| "working".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download_message(
        phase: BaseImageDownloadPhase,
        asset: &str,
        asset_index: u32,
        asset_count: u32,
    ) -> BaseImageDownloadProgress {
        BaseImageDownloadProgress {
            phase: phase.into(),
            asset: asset.to_string(),
            asset_index,
            asset_count,
            downloaded_bytes: 0,
            total_bytes: 0,
            message: String::new(),
            installed_path: String::new(),
        }
    }

    #[test]
    fn every_download_phase_names_itself_on_the_bar() {
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Verifying,
                "image.qcow2.zst.00.part",
                1,
                2
            )),
            "verifying"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Extracting,
                "stem",
                0,
                0
            )),
            "unpacking the image"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Installing,
                "stem",
                0,
                0
            )),
            "installing the image"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Resolving,
                "stem",
                0,
                0
            )),
            "resolving the build"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Done,
                "stem",
                0,
                0
            )),
            "done"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Unspecified,
                "stem",
                0,
                0
            )),
            "",
            "an unspecified phase must not overwrite the line already on screen"
        );
    }

    #[test]
    fn the_batch_position_only_appears_when_the_daemon_knows_it() {
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Downloading,
                "image.qcow2.zst.00.part",
                1,
                2
            )),
            "downloading part 1/2"
        );
        assert_eq!(
            download_bar_message(&download_message(
                BaseImageDownloadPhase::Downloading,
                "image.qcow2.zst.00.part",
                0,
                0
            )),
            "downloading",
            "mid-transfer messages carry no index, and (0/0) is noise"
        );
    }

    #[test]
    fn a_download_bar_line_stays_inside_the_terminal() {
        // The bar's own chrome — `[{elapsed}] [20-column bar] {bytes}/{total}
        // ({eta})` plus cliclack's symbol prefix — is about 50 columns, so
        // anything much above 25 here wraps the live frame and the next redraw
        // leaves the wrapped remainder behind.
        for phase in [
            BaseImageDownloadPhase::Downloading,
            BaseImageDownloadPhase::Verifying,
            BaseImageDownloadPhase::Extracting,
            BaseImageDownloadPhase::Installing,
            BaseImageDownloadPhase::Resolving,
            BaseImageDownloadPhase::Done,
        ] {
            let line = download_bar_message(&download_message(
                phase,
                "linux-waydroid-android13-vanilla-e2e1234.qcow2.zst.00.part",
                12,
                97,
            ));
            assert!(
                line.chars().count() <= 24,
                "{phase:?} renders {line:?}, which is too long for the bar's own chrome"
            );
        }
    }

    #[test]
    fn short_id_truncates_full_id_to_twelve_chars() {
        let full = "a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0a1b2";
        assert_eq!(short_id(full), "a1b2c3d4e5f6");
    }

    #[test]
    fn short_id_returns_input_unchanged_if_shorter_than_twelve() {
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn emit_json_emits_compact_json() {
        let value = serde_json::json!({ "instance_id": "abc", "n": 1 });
        // Guards the helper is wired to a working serializer; the compact
        // format itself is pinned by the E2E `--json` contract.
        assert!(emit_json(&value).is_ok());
    }

    #[test]
    fn colorize_status_returns_plain_text_when_not_a_tty() {
        assert_eq!(
            colorize_status(InstanceStateKind::Running, false),
            "Running"
        );
        assert_eq!(colorize_status(InstanceStateKind::Error, false), "Error");
    }

    #[test]
    fn colorize_status_wraps_in_ansi_codes_when_tty() {
        assert_eq!(
            colorize_status(InstanceStateKind::Running, true),
            "\x1b[32mRunning\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Error, true),
            "\x1b[31mError\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Paused, true),
            "\x1b[33mPaused\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Stopped, true),
            "\x1b[2mStopped\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Created, true),
            "\x1b[2mCreated\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Starting, true),
            "\x1b[36mStarting\x1b[0m"
        );
        assert_eq!(
            colorize_status(InstanceStateKind::Stopping, true),
            "\x1b[36mStopping\x1b[0m"
        );
    }

    #[test]
    fn colorize_status_unspecified_is_never_colored() {
        assert_eq!(
            colorize_status(InstanceStateKind::InstanceStateUnspecified, true),
            "UNSPECIFIED"
        );
    }

    #[test]
    fn ensure_qcow2_extension_adds_when_missing() {
        assert_eq!(
            ensure_qcow2_extension(std::path::Path::new("/home/user/my-disk")),
            std::path::PathBuf::from("/home/user/my-disk.qcow2")
        );
    }

    #[test]
    fn ensure_qcow2_extension_keeps_existing_extension() {
        assert_eq!(
            ensure_qcow2_extension(std::path::Path::new("/home/user/my-disk.img")),
            std::path::PathBuf::from("/home/user/my-disk.img")
        );
        assert_eq!(
            ensure_qcow2_extension(std::path::Path::new("/home/user/my-disk.raw")),
            std::path::PathBuf::from("/home/user/my-disk.raw")
        );
        assert_eq!(
            ensure_qcow2_extension(std::path::Path::new("/home/user/my-disk.qcow2")),
            std::path::PathBuf::from("/home/user/my-disk.qcow2")
        );
    }

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
        assert_eq!(parse_size("1T").unwrap(), 1024u64 * 1024 * 1024 * 1024);
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

    fn translator_operation(
        current_phase: &str,
        progress: f64,
    ) -> andler_rpc::proto::OperationInfo {
        andler_rpc::proto::OperationInfo {
            op_id: "op-1".to_string(),
            instance_id: "a1b2c3".to_string(),
            kind: "GuestInstall".to_string(),
            phases: vec![
                andler_rpc::proto::OperationPhase {
                    name: "downloading".to_string(),
                    weight: 0.5,
                },
                andler_rpc::proto::OperationPhase {
                    name: "staging".to_string(),
                    weight: 0.5,
                },
            ],
            progress,
            state: "Running".to_string(),
            error: String::new(),
            current_phase: current_phase.to_string(),
        }
    }

    /// The daemon skips `downloading` on a cache hit and stages straight away.
    /// 0.5 is exactly the weight boundary the derived name resolves to
    /// `downloading`, so the reported phase has to win.
    #[test]
    fn operation_phase_uses_the_phase_the_daemon_entered() {
        let op = translator_operation("staging", 0.5);
        assert_eq!(operation_phase(&op), "staging");
        assert_eq!(render_operation(&op), "GuestInstall — staging (50%)");
    }

    #[test]
    fn operation_phase_derives_the_phase_when_the_daemon_omits_it() {
        let op = translator_operation("", 0.5);
        assert_eq!(operation_phase(&op), "downloading");
    }
}
