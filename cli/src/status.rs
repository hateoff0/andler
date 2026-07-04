use andler_rpc::proto::andler_service_client::AndlerServiceClient;
use andler_rpc::proto::{
    instance_kind, network_mode, render_backend, AudioBackend, CpuPriority, DiskFormat,
    DisplayEngine, Empty, GetInstanceConfigResponse, InstanceIdRequest, LogStreamSource,
};
use tonic::transport::Channel;

use crate::helpers::{format_bytes, format_bytes_per_sec, state_kind_name};

pub async fn handle_status(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}

pub async fn handle_list(
    client: &mut AndlerServiceClient<Channel>,
    full_id: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client.list_instances(Empty {}).await?.into_inner();
    if response.instances.is_empty() {
        println!("no instances");
    } else {
        for entry in response.instances {
            // Shortened, Docker-`ps`-style prefix by default; any prefix
            // of this (down to a single hex char) is accepted by every
            // command that takes an `<instance_id>` — see
            // `Daemon::resolve_instance_id`. `--full-id`/`-q` prints the
            // full UUID for scripts.
            let id = if full_id {
                entry.instance_id.as_str()
            } else {
                short_id(&entry.instance_id)
            };
            println!("{}  {}  {}", id, state_kind_name(entry.state()), entry.name);
        }
    }
    Ok(())
}

/// First 8 characters of a full instance UUID string, matching `docker
/// ps`'s default short-id length. Falls back to the full string if it's
/// somehow already shorter (defensive only — `instance_id` is always a
/// full UUID coming from the daemon).
fn short_id(full: &str) -> &str {
    full.get(..8).unwrap_or(full)
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

pub async fn handle_logs(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}

pub async fn handle_metrics(
    client: &mut AndlerServiceClient<Channel>,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}

fn print_instance_config(config: GetInstanceConfigResponse) {
    println!("instance_id: {}", config.instance_id);
    println!("name: {}", config.name);
    println!("backend: {}", crate::helpers::backend_kind_name(config.backend()));

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
                andler_rpc::proto::AudioDevice::Unspecified => "UNSPECIFIED (defaults to virtio-sound)",
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
                // UNSPECIFIED falls back to the legacy tablet_mode bool —
                // see convert.rs — old daemons may still only send that.
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
    use super::short_id;

    #[test]
    fn short_id_truncates_full_uuid_to_eight_chars() {
        let full = "a1b2c3d4-e5f6-4789-a012-3456789abcde";
        assert_eq!(short_id(full), "a1b2c3d4");
    }

    #[test]
    fn short_id_returns_input_unchanged_if_shorter_than_eight() {
        assert_eq!(short_id("abc"), "abc");
    }
}
