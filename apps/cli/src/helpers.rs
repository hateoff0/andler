use andler_rpc::proto::{BackendKind, InstanceStateKind};
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

    loop {
        tokio::select! {
            result = &mut call => return result,
            _ = ticker.tick() => {
                let line = match active_operation(&mut poll, instance_ref).await {
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

/// The instance's running operation, rendered as one line, or `None`.
async fn active_operation(
    client: &mut crate::TracedClient,
    instance_ref: &str,
) -> Result<Option<String>, tonic::Status> {
    use andler_rpc::proto::Empty;

    let operations = client
        .list_operations(Empty {})
        .await?
        .into_inner()
        .operations;
    // The caller may hold a prefix of the id (`andler guest install libndk 7a`),
    // so match either direction.
    let op = operations.iter().find(|op| {
        op.instance_id.starts_with(instance_ref)
            || (!op.instance_id.is_empty() && instance_ref.starts_with(&op.instance_id))
    });

    Ok(op.map(|op| {
        format!(
            "{} — {} ({}%)",
            op.kind,
            operation_phase(op),
            (op.progress * 100.0).round() as u32
        )
    }))
}

/// The daemon publishes the phase list and the overall progress; the active
/// phase is the first one whose cumulative weight covers that progress, so the
/// CLI can name it without another field on the wire.
fn operation_phase(op: &andler_rpc::proto::OperationInfo) -> String {
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
}
