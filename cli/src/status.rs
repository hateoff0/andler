use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    instance_kind, network_mode, render_backend, AudioBackend, CpuPriority, DiskFormat,
    DisplayEngine, Empty, GetInstanceConfigResponse, InstanceIdRequest, InstanceStateKind,
    LogStreamSource,
};
use std::io::IsTerminal;
use tonic::transport::Channel;

use crate::helpers::{
    colorize_status, format_bytes, format_bytes_per_sec, format_size, state_kind_name,
};
use crate::{CliLogSource, ListSortKey};

pub async fn handle_status(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_instance_status(InstanceIdRequest {
            instance_id: instance_id.clone(),
        })
        .await?
        .into_inner();
    let state = response.state();
    if json {
        #[derive(serde::Serialize)]
        struct InstanceStatusJson<'a> {
            instance_id: &'a str,
            state: &'a str,
            #[serde(skip_serializing_if = "str::is_empty")]
            detail: &'a str,
            #[serde(skip_serializing_if = "str::is_empty")]
            error_message: &'a str,
        }
        println!(
            "{}",
            serde_json::to_string(&InstanceStatusJson {
                instance_id: &instance_id,
                state: state_kind_name(state),
                detail: &response.detail,
                error_message: &response.error_message,
            })?
        );
        return Ok(());
    }
    println!(
        "state: {}",
        colorize_status(state, std::io::stdout().is_terminal())
    );
    if !response.detail.is_empty() {
        println!("detail: {}", response.detail);
    }
    if !response.error_message.is_empty() {
        println!("error: {}", response.error_message);
    }
    Ok(())
}

fn parse_state_filter(s: &str) -> Option<InstanceStateKind> {
    [
        InstanceStateKind::Created,
        InstanceStateKind::Starting,
        InstanceStateKind::Running,
        InstanceStateKind::Paused,
        InstanceStateKind::Stopping,
        InstanceStateKind::Stopped,
        InstanceStateKind::Error,
    ]
    .into_iter()
    .find(|&kind| state_kind_name(kind).eq_ignore_ascii_case(s))
}

#[derive(serde::Serialize)]
struct InstanceListJson<'a> {
    id: &'a str,
    name: &'a str,
    state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<&'a str>,
}

pub async fn handle_list(
    client: &mut AndlerServiceClient<Channel>,
    full_id: bool,
    state: Option<String>,
    name: Option<String>,
    sort: ListSortKey,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_filter = match state {
        Some(raw) => match parse_state_filter(&raw) {
            Some(kind) => Some(kind),
            None => {
                eprintln!(
                    "unknown state {raw:?} (expected one of: Created, Starting, Running, \
                     Paused, Stopping, Stopped, Error)"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let name_filter = match name {
        Some(pattern) => match regex::Regex::new(&pattern) {
            Ok(re) => Some(re),
            Err(err) => {
                eprintln!("invalid --name regex {pattern:?}: {err}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    let response = client.list_instances(Empty {}).await?.into_inner();
    let is_tty = std::io::stdout().is_terminal();

    let mut instances: Vec<_> = response
        .instances
        .into_iter()
        .filter(|entry| state_filter.is_none_or(|want| entry.state() == want))
        .filter(|entry| {
            name_filter
                .as_ref()
                .is_none_or(|re| re.is_match(&entry.name))
        })
        .collect();

    match sort {
        ListSortKey::None => {}
        ListSortKey::Name => instances.sort_by(|a, b| a.name.cmp(&b.name)),
        ListSortKey::State => {
            instances.sort_by_key(|entry| state_kind_name(entry.state()).to_string())
        }
    }

    if json {
        let entries: Vec<InstanceListJson> = instances
            .iter()
            .map(|entry| InstanceListJson {
                id: &entry.instance_id,
                name: &entry.name,
                state: state_kind_name(entry.state()),
                error_message: (!entry.error_message.is_empty())
                    .then_some(entry.error_message.as_str()),
            })
            .collect();
        println!("{}", serde_json::to_string(&entries)?);
        return Ok(());
    }

    if instances.is_empty() {
        println!("no instances");
    } else {
        for entry in instances {
            let id = if full_id {
                entry.instance_id.as_str()
            } else {
                crate::helpers::short_id(&entry.instance_id)
            };
            print!(
                "{}  {}  {}",
                id,
                colorize_status(entry.state(), is_tty),
                entry.name
            );
            if entry.state() == InstanceStateKind::Error && !entry.error_message.is_empty() {
                print!("  ({})", entry.error_message);
            }
            println!();
        }
    }
    Ok(())
}

pub async fn handle_config(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_instance_config(InstanceIdRequest { instance_id })
        .await?
        .into_inner();
    print_instance_config(response);
    Ok(())
}

fn log_line_matches_filters(
    line: &andler_rpc::proto::LogLineResponse,
    source: &Option<CliLogSource>,
    grep_re: &Option<regex::Regex>,
) -> bool {
    let source_ok = match source {
        None => true,
        Some(CliLogSource::Stdout) => line.source() == LogStreamSource::Stdout,
        Some(CliLogSource::Stderr) => line.source() == LogStreamSource::Stderr,
    };
    source_ok && grep_re.as_ref().is_none_or(|re| re.is_match(&line.line))
}

pub async fn handle_logs(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    source: Option<CliLogSource>,
    grep: Option<String>,
    tail: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let grep_re = match grep {
        Some(pattern) => match regex::Regex::new(&pattern) {
            Ok(re) => Some(re),
            Err(err) => {
                eprintln!("invalid --grep regex {pattern:?}: {err}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    let mut stream = client
        .stream_instance_logs(InstanceIdRequest { instance_id })
        .await?
        .into_inner();

    let print_line = |line: &andler_rpc::proto::LogLineResponse| {
        let prefix = match line.source() {
            LogStreamSource::Stdout => "stdout",
            LogStreamSource::Stderr => "stderr",
            LogStreamSource::Unspecified => "unspecified",
        };
        println!("[{prefix}] {}", line.line);
    };

    let mut got_any_line = false;

    if let Some(n) = tail {
        let mut ring: std::collections::VecDeque<andler_rpc::proto::LogLineResponse> =
            std::collections::VecDeque::with_capacity(n.min(10_000));
        const IDLE_GAP: std::time::Duration = std::time::Duration::from_millis(300);

        loop {
            match tokio::time::timeout(IDLE_GAP, stream.message()).await {
                Ok(Ok(Some(line))) => {
                    got_any_line = true;
                    if log_line_matches_filters(&line, &source, &grep_re) {
                        if ring.len() == n {
                            ring.pop_front();
                        }
                        if n > 0 {
                            ring.push_back(line);
                        }
                    }
                }
                Ok(Ok(None)) => break, // stream closed before any gap — flush and stop below
                Ok(Err(status)) => return Err(status.into()),
                Err(_elapsed) => break, // idle gap observed — assume history is done
            }
        }
        for line in ring.drain(..) {
            print_line(&line);
        }
    }

    while let Some(line) = stream.message().await? {
        got_any_line = true;
        if log_line_matches_filters(&line, &source, &grep_re) {
            print_line(&line);
        }
    }

    if !got_any_line {
        eprintln!(
            "no log lines received (instance may have no running backend right now, \
             or simply hasn't written anything to stdout/stderr yet)"
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct MetricsJson {
    cpu_percent: Option<f32>,
    rss_bytes: Option<u64>,
    disk_read_bytes_sec: Option<u64>,
    disk_write_bytes_sec: Option<u64>,
    net_rx_bytes_sec: Option<u64>,
    net_tx_bytes_sec: Option<u64>,
    vram_used_bytes: Option<u64>,
    vram_total_bytes: Option<u64>,
    gpu_load_percent: Option<f32>,
}

pub async fn handle_metrics(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
    once: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = client
        .stream_resource_metrics(InstanceIdRequest { instance_id })
        .await?
        .into_inner();

    let mut got_any_sample = false;
    while let Some(m) = stream.message().await? {
        got_any_sample = true;

        if json {
            let sample = MetricsJson {
                cpu_percent: m.cpu_percent,
                rss_bytes: m.memory_used_bytes,
                disk_read_bytes_sec: m.disk_read_bytes_per_sec,
                disk_write_bytes_sec: m.disk_write_bytes_per_sec,
                net_rx_bytes_sec: m.net_rx_bytes_per_sec,
                net_tx_bytes_sec: m.net_tx_bytes_per_sec,
                vram_used_bytes: m.vram_used_bytes,
                vram_total_bytes: m.vram_total_bytes,
                gpu_load_percent: m.gpu_load_percent,
            };
            println!("{}", serde_json::to_string(&sample)?);
        } else {
            let cpu = m
                .cpu_percent
                .map(|v| format!("{:.1}%", v))
                .unwrap_or_else(|| "N/A".to_string());
            let rss = m
                .memory_used_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "N/A".to_string());
            let dr = m
                .disk_read_bytes_per_sec
                .map(format_bytes_per_sec)
                .unwrap_or_else(|| "N/A".to_string());
            let dw = m
                .disk_write_bytes_per_sec
                .map(format_bytes_per_sec)
                .unwrap_or_else(|| "N/A".to_string());
            let nr = m
                .net_rx_bytes_per_sec
                .map(format_bytes_per_sec)
                .unwrap_or_else(|| "N/A".to_string());
            let nt = m
                .net_tx_bytes_per_sec
                .map(format_bytes_per_sec)
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

        if once {
            break;
        }
    }

    if !got_any_sample {
        eprintln!("no metrics received (instance may have no running backend right now)");
    }
    Ok(())
}

fn print_instance_config(config: GetInstanceConfigResponse) {
    println!(
        "instance_id: {}",
        crate::helpers::short_id(&config.instance_id)
    );
    println!("name: {}", config.name);
    println!(
        "backend: {}",
        crate::helpers::backend_kind_name(config.backend())
    );

    match config.kind.and_then(|k| k.kind) {
        Some(instance_kind::Kind::LinuxVm(linux_vm)) => {
            println!("kind: LinuxVm");
            println!("  iso_path: {}", linux_vm.iso_path);
            println!("  cdrom_bus: {:?}", linux_vm.cdrom_bus());
        }
        Some(instance_kind::Kind::AndroidVm(android_vm)) => {
            println!("kind: AndroidVm");
            if let Some(profile) = android_vm.android_profile {
                println!("  android_version: {:?}", profile.android_version());
                println!("  gapps: {}", profile.gapps);
                println!("  microg: {}", profile.microg);
                println!("  arm_translator: {:?}", profile.arm_translator());
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
        println!("  size_bytes: {}", format_size(memory.size_bytes));
        println!("  ballooning: {}", memory.ballooning);
        println!("  zram: {}", memory.zram);
        println!("  ksm: {}", memory.ksm);
    }

    if let Some(disk) = config.disk {
        println!("[disk]");
        println!("  path: {}", disk.path);
        println!("  size_bytes: {}", format_size(disk.size_bytes));
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
        println!("  compact_on_shutdown: {}", disk.compact_on_shutdown);
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
                DisplayEngine::Gtk => "Gtk",
            }
        );
        println!("  fullscreen: {}", display.fullscreen);
    }

    if let Some(gpu) = config.gpu {
        println!("[gpu]");
        println!("  hostmem_bytes: {}", format_size(gpu.hostmem_bytes));
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
        match network.mode.clone().and_then(|m| m.kind) {
            Some(network_mode::Kind::Nat(_)) => {
                let nat_backend = match network.nat_backend() {
                    andler_rpc::proto::NatBackend::Passt => "passt",
                    _ => "slirp",
                };
                println!("  mode: Nat ({nat_backend})")
            }
            Some(network_mode::Kind::Bridge(b)) => {
                println!("  mode: Bridge({})", b.interface)
            }
            Some(network_mode::Kind::Isolated(_)) => println!("  mode: Isolated"),
            None => println!("  mode: <missing>"),
        }
    }

    if let Some(firmware) = config.firmware {
        println!("[firmware]");
        if firmware.ovmf_code_path.is_empty() {
            println!("  ovmf_code_path: (resolved by daemon at start)");
        } else {
            println!("  ovmf_code_path: {}", firmware.ovmf_code_path);
        }
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
        println!(
            "  device: {}",
            match audio.device() {
                andler_rpc::proto::AudioDevice::Unspecified =>
                    "UNSPECIFIED (defaults to virtio-sound)",
                andler_rpc::proto::AudioDevice::VirtioSound => "virtio-sound",
                andler_rpc::proto::AudioDevice::Ich9Hda => "ich9-hda",
            }
        );
    }

    if let Some(input) = config.input {
        println!("[input]");
        println!(
            "  pointer_mode: {}",
            match input.pointer_mode() {
                andler_rpc::proto::PointerMode::Mouse => "mouse",
                andler_rpc::proto::PointerMode::Unspecified if !input.tablet_mode => "mouse",
                _ => "tablet",
            }
        );
        println!("  hide_host_cursor: {}", input.hide_host_cursor);
        println!("  clipboard_enabled: {}", input.clipboard_enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::{log_line_matches_filters, parse_state_filter, MetricsJson};
    use crate::CliLogSource;
    use andler_rpc::proto::{InstanceStateKind, LogLineResponse, LogStreamSource};

    fn line(source: LogStreamSource, text: &str) -> LogLineResponse {
        let mut msg = LogLineResponse {
            line: text.to_string(),
            ..Default::default()
        };
        msg.set_source(source);
        msg
    }

    #[test]
    fn log_filter_no_filters_matches_everything() {
        let l = line(LogStreamSource::Stdout, "anything at all");
        assert!(log_line_matches_filters(&l, &None, &None));
    }

    #[test]
    fn log_filter_source_stdout_excludes_stderr() {
        let stdout_line = line(LogStreamSource::Stdout, "hello");
        let stderr_line = line(LogStreamSource::Stderr, "hello");
        let filter = Some(CliLogSource::Stdout);
        assert!(log_line_matches_filters(&stdout_line, &filter, &None));
        assert!(!log_line_matches_filters(&stderr_line, &filter, &None));
    }

    #[test]
    fn log_filter_source_stderr_excludes_stdout() {
        let stdout_line = line(LogStreamSource::Stdout, "hello");
        let stderr_line = line(LogStreamSource::Stderr, "hello");
        let filter = Some(CliLogSource::Stderr);
        assert!(!log_line_matches_filters(&stdout_line, &filter, &None));
        assert!(log_line_matches_filters(&stderr_line, &filter, &None));
    }

    #[test]
    fn log_filter_grep_matches_pattern() {
        let re = Some(regex::Regex::new("error|warning").unwrap());
        let matching = line(LogStreamSource::Stderr, "a warning occurred");
        let non_matching = line(LogStreamSource::Stdout, "all good here");
        assert!(log_line_matches_filters(&matching, &None, &re));
        assert!(!log_line_matches_filters(&non_matching, &None, &re));
    }

    #[test]
    fn log_filter_source_and_grep_combine_with_and() {
        let re = Some(regex::Regex::new("error").unwrap());
        let filter = Some(CliLogSource::Stderr);
        let l1 = line(LogStreamSource::Stderr, "all fine");
        assert!(!log_line_matches_filters(&l1, &filter, &re));
        let l2 = line(LogStreamSource::Stdout, "an error happened");
        assert!(!log_line_matches_filters(&l2, &filter, &re));
        let l3 = line(LogStreamSource::Stderr, "an error happened");
        assert!(log_line_matches_filters(&l3, &filter, &re));
    }

    #[test]
    fn parse_state_filter_is_case_insensitive() {
        assert_eq!(
            parse_state_filter("running"),
            Some(InstanceStateKind::Running)
        );
        assert_eq!(
            parse_state_filter("RUNNING"),
            Some(InstanceStateKind::Running)
        );
        assert_eq!(
            parse_state_filter("Running"),
            Some(InstanceStateKind::Running)
        );
    }

    #[test]
    fn parse_state_filter_covers_every_real_state() {
        for (input, expected) in [
            ("Created", InstanceStateKind::Created),
            ("Starting", InstanceStateKind::Starting),
            ("Paused", InstanceStateKind::Paused),
            ("Stopping", InstanceStateKind::Stopping),
            ("Stopped", InstanceStateKind::Stopped),
            ("Error", InstanceStateKind::Error),
        ] {
            assert_eq!(parse_state_filter(input), Some(expected));
        }
    }

    #[test]
    fn parse_state_filter_rejects_unknown_input() {
        assert_eq!(parse_state_filter("bogus"), None);
        assert_eq!(parse_state_filter(""), None);
    }

    #[test]
    fn metrics_json_serializes_expected_field_names() {
        let sample = MetricsJson {
            cpu_percent: Some(12.3),
            rss_bytes: Some(2_254_857_830),
            disk_read_bytes_sec: Some(0),
            disk_write_bytes_sec: Some(0),
            net_rx_bytes_sec: Some(1_258_291),
            net_tx_bytes_sec: Some(314_572),
            vram_used_bytes: None,
            vram_total_bytes: None,
            gpu_load_percent: None,
        };
        let json = serde_json::to_string(&sample).unwrap();
        assert!(json.contains("\"cpu_percent\":12.3"));
        assert!(json.contains("\"rss_bytes\":2254857830"));
        assert!(json.contains("\"vram_used_bytes\":null"));
    }
}
