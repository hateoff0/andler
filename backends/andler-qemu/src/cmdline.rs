use std::path::Path;

use andler_core::{
    AudioBackend, AudioDevice, BackendError, CdromBus, DiskFormat, DisplayEngine, InstanceConfig,
    InstanceKind, NatBackend, NetworkMode, PointerMode, RenderBackend,
};

pub fn build_args(
    cfg: &InstanceConfig,
    qmp_socket_path: &Path,
) -> Result<Vec<String>, BackendError> {
    let mut args = Vec::new();
    args.extend(name_args(cfg));
    args.extend(machine_and_cpu_args(cfg));
    args.extend(memory_args(cfg));
    args.extend(firmware_args(cfg));
    args.extend(gpu_display_args(cfg)?);
    if let Some(fwcfg_args) = display_resolution_fwcfg_args(cfg) {
        args.extend(fwcfg_args);
    }
    args.extend(disk_args(cfg));
    args.extend(input_args(cfg));
    args.extend(network_args(cfg));
    args.extend(audio_args(cfg));
    args.extend(qmp_args(qmp_socket_path));
    args.extend(guest_agent_args(cfg, qmp_socket_path));
    args.extend(serial_args(cfg));
    args.push("-boot".to_string());
    args.push("menu=on".to_string());
    Ok(args)
}

fn name_args(cfg: &InstanceConfig) -> Vec<String> {
    vec![
        "-name".to_string(),
        format!("{},process={}", cfg.name, cfg.name),
    ]
}

fn qmp_args(qmp_socket_path: &Path) -> Vec<String> {
    vec![
        "-qmp".to_string(),
        format!("unix:{},server,nowait", qmp_socket_path.display()),
    ]
}

/// QEMU ids for hotplugged block devices, derived from the device's position in
/// `InstanceConfig::extra_disks`. Both `cmdline.rs` (boot-time re-attach) and the
/// QMP hotplug path must agree on these so a device attached at runtime comes
/// back with the same ids after a restart.
pub fn extra_disk_drive_id(index: usize) -> String {
    format!("drive-extra{index}")
}

pub fn extra_disk_device_id(index: usize) -> String {
    format!("extra{index}")
}

/// QEMU netdev id (the NIC device shares it) for a hotplugged NIC at position
/// `index` in `InstanceConfig::extra_networks`.
pub fn extra_net_id(index: usize) -> String {
    format!("net-extra{index}")
}

/// Host-side tap interface name for the primary bridged NIC. Linux caps
/// interface names at IFNAMSIZ (15 chars), so only the first 8 hex chars of
/// the id (the CLI's short-id form) fit — a full 64-hex id would be rejected
/// by `ip link add` and QEMU's TUNSETIFF.
pub fn primary_net_bridge_tap_iface(instance_id: &str) -> String {
    let short = &instance_id[..instance_id.len().min(8)];
    format!("tap{short}")
}

/// Host-side tap interface name for a hotplugged bridge NIC. The instance id is
/// part of the name because multiple instances may share one host bridge — the
/// primary NIC uses `tap{short-id}`, extras use `tap{short-id}-e{index}`.
/// Linux caps interface names at IFNAMSIZ (15 chars), so only the first 8 hex
/// chars of the id (the CLI's short-id form) fit.
pub fn extra_net_bridge_tap_iface(instance_id: &str, index: usize) -> String {
    let short = &instance_id[..instance_id.len().min(8)];
    format!("tap{short}-e{index}")
}

/// Host-side veth endpoint name for a hotplugged isolated NIC.
pub fn extra_net_isolated_iface(index: usize) -> String {
    format!("andler-e{index}")
}

fn guest_agent_args(cfg: &InstanceConfig, qmp_socket_path: &Path) -> Vec<String> {
    let qga_socket_path = qmp_socket_path.with_extension("qga.sock");
    let mut args = vec![
        "-chardev".to_string(),
        format!(
            "socket,id=qga,path={},server=on,wait=off",
            qga_socket_path.display()
        ),
    ];
    // The clipboard path (input_args) already adds a virtio-serial-pci bus
    // when clipboard_enabled; the agent port attaches to that one then.
    if !cfg.input.clipboard_enabled {
        args.push("-device".to_string());
        args.push("virtio-serial-pci".to_string());
    }
    args.push("-device".to_string());
    args.push("virtserialport,chardev=qga,id=qga,name=org.qemu.guest_agent.0".to_string());
    args
}

fn serial_args(cfg: &InstanceConfig) -> Vec<String> {
    let Some(instance_dir) = cfg.disk.path.parent() else {
        return Vec::new();
    };
    // The serial console is an attachable unix socket (`andler connect
    // --level console`); the chardev's logfile keeps console.log as the
    // historical serial log alongside live attach sessions.
    let console_log_path = instance_dir.join("console.log");
    let console_socket_path = andler_core::paths::console_socket_path(&cfg.id);
    vec![
        "-chardev".to_string(),
        format!(
            "socket,id=console0,path={},server=on,wait=off,logfile={},logappend=on",
            console_socket_path.display(),
            console_log_path.display()
        ),
        "-serial".to_string(),
        "chardev:console0".to_string(),
    ]
}

fn machine_and_cpu_args(cfg: &InstanceConfig) -> Vec<String> {
    let cpu = &cfg.cpu;
    let mut args = vec!["-machine".to_string(), "q35,accel=kvm,usb=on".to_string()];
    // q35's root bus rejects device_add; hotplug works through pcie-root-port
    // bridges, one per attachable slot: 0..8 for extra disks, 8..16 for nets.
    for slot in 0..16 {
        args.push("-device".to_string());
        args.push(format!(
            "pcie-root-port,id=root-port-{slot},chassis={},slot={}",
            slot + 1,
            slot
        ));
    }
    args.extend([
        "-cpu".to_string(),
        "host,kvm=on,+topoext,migratable=no".to_string(),
        "-smp".to_string(),
        format!(
            "cpus={},sockets={},dies=1,cores={},threads={}",
            cpu.cores, cpu.sockets, cpu.cores, cpu.threads
        ),
    ]);
    args
}

fn memory_args(cfg: &InstanceConfig) -> Vec<String> {
    let size = qemu_size_suffix(cfg.memory.size_bytes);
    let share = if cfg.memory.ksm { "on" } else { "off" };
    vec![
        "-m".to_string(),
        size.clone(),
        "-object".to_string(),
        format!("memory-backend-memfd,id=mem1,size={size},share={share}"),
        "-machine".to_string(),
        "memory-backend=mem1".to_string(),
    ]
}

fn firmware_args(cfg: &InstanceConfig) -> Vec<String> {
    if !cfg.firmware.enable_uefi {
        return vec![];
    }
    let fw = &cfg.firmware;
    vec![
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,readonly=on,file={}",
            fw.ovmf_code_path.display()
        ),
        "-drive".to_string(),
        format!("if=pflash,format=raw,file={}", fw.ovmf_vars_path.display()),
    ]
}

fn gpu_display_args(cfg: &InstanceConfig) -> Result<Vec<String>, BackendError> {
    let gpu = &cfg.gpu;
    let mut args = Vec::new();

    match &gpu.render_backend {
        RenderBackend::Cpu => {
            args.push("-vga".to_string());
            args.push("std".to_string());
        }
        RenderBackend::VirtioGpu => {
            args.push("-vga".to_string());
            args.push("none".to_string());
            args.push("-device".to_string());
            args.push("virtio-gpu-pci".to_string());
        }
        RenderBackend::VirGl => {
            args.push("-vga".to_string());
            args.push("none".to_string());
            args.push("-device".to_string());
            args.push(format!(
                "virtio-gpu-gl,hostmem={},blob={}",
                qemu_size_suffix(gpu.hostmem_bytes),
                gpu.blob
            ));
        }
        RenderBackend::Venus => {
            args.push("-vga".to_string());
            args.push("none".to_string());
            args.push("-device".to_string());
            args.push(format!(
                "virtio-gpu-gl,hostmem={},blob={},venus=true",
                qemu_size_suffix(gpu.hostmem_bytes),
                gpu.blob
            ));
        }
        RenderBackend::Passthrough { .. } => {
            return Err(BackendError::NotImplemented {
                backend: "qemu",
                operation: "RenderBackend::Passthrough",
            });
        }
    }

    let show_cursor = if cfg.input.hide_host_cursor {
        "off"
    } else {
        "on"
    };
    let display_str = match cfg.display.display_engine {
        DisplayEngine::Sdl => format!(
            "sdl,gl={},show-cursor={},window-close=off",
            if gpu.gl { "on" } else { "off" },
            show_cursor
        ),
        DisplayEngine::Gtk => format!(
            "gtk,gl={},show-cursor={},clipboard=on,window-close=off",
            if gpu.gl { "on" } else { "off" },
            show_cursor
        ),
        DisplayEngine::Spice => "spice-app".to_string(),
        DisplayEngine::Dbus => "dbus".to_string(),
        DisplayEngine::None => "none".to_string(),
    };
    args.push("-display".to_string());
    args.push(display_str);

    Ok(args)
}

fn display_resolution_fwcfg_args(cfg: &InstanceConfig) -> Option<Vec<String>> {
    let res = cfg.display.resolution;
    (res.width > 0 && res.height > 0).then(|| {
        vec![
            "-fw_cfg".to_string(),
            format!(
                "name=opt/andler/display-resolution,string={}x{}",
                res.width, res.height
            ),
        ]
    })
}

fn disk_args(cfg: &InstanceConfig) -> Vec<String> {
    let disk = &cfg.disk;

    let mut args = Vec::new();

    // Main disk uses explicit -blockdev node names: DiskChain live snapshots
    // and commits address the graph by node (`drive-disk0`), which a
    // `-drive if=none,id=`-style backend cannot name. The file layer carries
    // discard (unmap) so host-side hole punching survives the chain.
    match disk.format {
        DiskFormat::Qcow2 => {
            args.push("-blockdev".to_string());
            args.push(format!(
                "driver=file,node-name=file-disk0,filename={},aio=threads,discard=unmap",
                disk.path.display()
            ));
            args.push("-blockdev".to_string());
            args.push(
                "driver=qcow2,node-name=drive-disk0,file=file-disk0,detect-zeroes=on".to_string(),
            );
        }
        DiskFormat::Raw => {
            args.push("-blockdev".to_string());
            args.push(format!(
                "driver=file,node-name=file-disk0,filename={},aio=threads,discard=unmap",
                disk.path.display()
            ));
            args.push("-blockdev".to_string());
            args.push("driver=raw,node-name=drive-disk0,file=file-disk0".to_string());
        }
        DiskFormat::Vdi => {
            args.push("-blockdev".to_string());
            args.push(format!(
                "driver=file,node-name=file-disk0,filename={},aio=threads",
                disk.path.display()
            ));
            args.push("-blockdev".to_string());
            args.push("driver=vdi,node-name=drive-disk0,file=file-disk0".to_string());
        }
    }
    args.push("-device".to_string());
    args.push("virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4".to_string());

    for (index, extra) in cfg.extra_disks.iter().enumerate() {
        let extra_format = match extra.format {
            DiskFormat::Qcow2 => "qcow2",
            DiskFormat::Raw => "raw",
            DiskFormat::Vdi => "vdi",
        };
        let drive_id = extra_disk_drive_id(index);
        let device_id = extra_disk_device_id(index);
        args.push("-drive".to_string());
        args.push(format!(
            "file={},format={},if=none,id={},discard=on,detect-zeroes=on,aio=threads",
            extra.path.display(),
            extra_format,
            drive_id
        ));
        args.push("-device".to_string());
        args.push(format!(
            "virtio-blk-pci,drive={drive_id},id={device_id},bus=root-port-{}",
            index % 8
        ));
    }

    if let InstanceKind::LinuxVm {
        iso_path,
        cdrom_bus,
    } = &cfg.kind
    {
        args.push("-drive".to_string());
        args.push(format!(
            "file={},media=cdrom,if=none,id=drive-cd0",
            iso_path.display()
        ));
        match cdrom_bus {
            CdromBus::Ide => {
                args.push("-device".to_string());
                args.push("ide-cd,drive=drive-cd0,id=cd0,bootindex=2".to_string());
            }
            CdromBus::VirtioScsi => {
                args.push("-device".to_string());
                args.push("virtio-scsi-pci,id=scsi0".to_string());
                args.push("-device".to_string());
                args.push("scsi-cd,drive=drive-cd0,bus=scsi0.0,id=cd0,bootindex=2".to_string());
            }
        }
    }

    args
}

fn input_args(cfg: &InstanceConfig) -> Vec<String> {
    let input = &cfg.input;
    let mut args = Vec::new();

    match input.pointer_mode {
        PointerMode::Tablet => {
            args.push("-device".to_string());
            args.push("virtio-tablet-pci,id=tablet0".to_string());
        }
        PointerMode::Mouse => {
            args.push("-device".to_string());
            args.push("virtio-mouse-pci,id=mouse0".to_string());
        }
    }

    if input.clipboard_enabled {
        args.push("-device".to_string());
        args.push("virtio-serial-pci".to_string());
        args.push("-device".to_string());
        args.push("virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0".to_string());
        args.push("-chardev".to_string());
        args.push("qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on".to_string());
    }

    args
}

fn network_args(cfg: &InstanceConfig) -> Vec<String> {
    let mut args = primary_network_args(cfg);

    for (index, extra) in cfg.extra_networks.iter().enumerate() {
        let netdev_id = extra_net_id(index);
        let netdev = match &extra.mode {
            NetworkMode::Nat => match extra.nat_backend {
                NatBackend::Slirp => format!("user,id={netdev_id}"),
                NatBackend::Passt => format!("passt,id={netdev_id}"),
            },
            NetworkMode::Bridge { interface: bridge } => {
                let tap_iface = extra_net_bridge_tap_iface(&cfg.id.to_string(), index);
                format!(
                    "tap,id={netdev_id},ifname={tap_iface},bridge={bridge},script=no,downscript=no"
                )
            }
            NetworkMode::Isolated => {
                let tap_iface = extra_net_isolated_iface(index);
                format!("tap,id={netdev_id},ifname={tap_iface},script=no,downscript=no")
            }
        };
        args.push("-netdev".to_string());
        args.push(netdev);
        args.push("-device".to_string());
        args.push(format!(
            "{},netdev={netdev_id},bus=root-port-{}",
            extra.device_model,
            8 + index % 8
        ));
    }

    args
}

fn primary_network_args(cfg: &InstanceConfig) -> Vec<String> {
    match &cfg.network.mode {
        NetworkMode::Nat => match cfg.network.nat_backend {
            NatBackend::Slirp => vec![
                "-nic".to_string(),
                format!("user,model={}", cfg.network.device_model),
            ],
            NatBackend::Passt => vec![
                "-netdev".to_string(),
                "passt,id=net0".to_string(),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ],
        },
        NetworkMode::Bridge { interface: bridge } => {
            let tap_iface = primary_net_bridge_tap_iface(&cfg.id.to_string());
            vec![
                "-netdev".to_string(),
                format!(
                    "tap,id=net0,ifname={},bridge={},script=no,downscript=no",
                    tap_iface, bridge
                ),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        }
        NetworkMode::Isolated => {
            let vm_iface = "andler0";
            vec![
                "-netdev".to_string(),
                format!("tap,id=net0,ifname={},script=no,downscript=no", vm_iface),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        }
    }
}

fn audio_args(cfg: &InstanceConfig) -> Vec<String> {
    match cfg.audio.backend {
        AudioBackend::None => Vec::new(),
        AudioBackend::Pipewire | AudioBackend::Pulseaudio => {
            let backend_str = match cfg.audio.backend {
                AudioBackend::Pipewire => "pipewire",
                AudioBackend::Pulseaudio => "pulseaudio",
                AudioBackend::None => unreachable!(),
            };
            let mut args = vec!["-audiodev".to_string(), format!("{backend_str},id=snd0")];
            match cfg.audio.device {
                AudioDevice::VirtioSound => {
                    args.push("-device".to_string());
                    args.push("virtio-sound-pci,audiodev=snd0".to_string());
                }
                AudioDevice::Ich9Hda => {
                    args.push("-device".to_string());
                    args.push("ich9-intel-hda".to_string());
                    args.push("-device".to_string());
                    args.push("hda-output,audiodev=snd0".to_string());
                }
            }
            args
        }
    }
}

fn qemu_size_suffix(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    if bytes.is_multiple_of(GIB) {
        format!("{}G", bytes / GIB)
    } else {
        format!("{}M", bytes / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
        InputConfig, InstanceConfig, InstanceId, MemoryConfig, NetworkConfig, NetworkMode,
    };
    use std::path::PathBuf;

    fn start_sh_equivalent_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "linux".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("cachyos-desktop-linux-260426.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            schema_version: andler_core::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("linux_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[test]
    fn name_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(name_args(&cfg), vec!["-name", "linux,process=linux"]);
    }

    #[test]
    fn machine_and_cpu_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            machine_and_cpu_args(&cfg),
            vec![
                "-machine",
                "q35,accel=kvm,usb=on",
                "-device",
                "pcie-root-port,id=root-port-0,chassis=1,slot=0",
                "-device",
                "pcie-root-port,id=root-port-1,chassis=2,slot=1",
                "-device",
                "pcie-root-port,id=root-port-2,chassis=3,slot=2",
                "-device",
                "pcie-root-port,id=root-port-3,chassis=4,slot=3",
                "-device",
                "pcie-root-port,id=root-port-4,chassis=5,slot=4",
                "-device",
                "pcie-root-port,id=root-port-5,chassis=6,slot=5",
                "-device",
                "pcie-root-port,id=root-port-6,chassis=7,slot=6",
                "-device",
                "pcie-root-port,id=root-port-7,chassis=8,slot=7",
                "-device",
                "pcie-root-port,id=root-port-8,chassis=9,slot=8",
                "-device",
                "pcie-root-port,id=root-port-9,chassis=10,slot=9",
                "-device",
                "pcie-root-port,id=root-port-10,chassis=11,slot=10",
                "-device",
                "pcie-root-port,id=root-port-11,chassis=12,slot=11",
                "-device",
                "pcie-root-port,id=root-port-12,chassis=13,slot=12",
                "-device",
                "pcie-root-port,id=root-port-13,chassis=14,slot=13",
                "-device",
                "pcie-root-port,id=root-port-14,chassis=15,slot=14",
                "-device",
                "pcie-root-port,id=root-port-15,chassis=16,slot=15",
                "-cpu",
                "host,kvm=on,+topoext,migratable=no",
                "-smp",
                "cpus=4,sockets=1,dies=1,cores=4,threads=1",
            ]
        );
    }

    #[test]
    fn memory_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            memory_args(&cfg),
            vec![
                "-m",
                "8G",
                "-object",
                "memory-backend-memfd,id=mem1,size=8G,share=on",
                "-machine",
                "memory-backend=mem1",
            ]
        );
    }

    #[test]
    fn memory_args_disable_share_when_ksm_off() {
        let mut cfg = start_sh_equivalent_config();
        cfg.memory.ksm = false;
        let args = memory_args(&cfg);
        assert!(args.contains(&"memory-backend-memfd,id=mem1,size=8G,share=off".to_string()));
    }

    #[test]
    fn firmware_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            firmware_args(&cfg),
            vec![
                "-drive",
                "if=pflash,format=raw,readonly=on,file=/usr/share/edk2/x64/OVMF_CODE.4m.fd",
                "-drive",
                "if=pflash,format=raw,file=linux_VARS.fd",
            ]
        );
    }

    #[test]
    fn firmware_args_empty_when_uefi_disabled() {
        let mut cfg = start_sh_equivalent_config();
        cfg.firmware.enable_uefi = false;
        assert!(firmware_args(&cfg).is_empty());
    }

    #[test]
    fn gpu_display_args_match_start_sh_for_venus() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            gpu_display_args(&cfg).unwrap(),
            vec![
                "-vga",
                "none",
                "-device",
                "virtio-gpu-gl,hostmem=4G,blob=true,venus=true",
                "-display",
                "sdl,gl=on,show-cursor=off,window-close=off",
            ]
        );
    }

    #[test]
    fn gpu_display_args_for_cpu_backend_uses_vga_std() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::Cpu;
        let args = gpu_display_args(&cfg).unwrap();
        assert_eq!(&args[0..2], &["-vga".to_string(), "std".to_string()]);
        assert!(!args.iter().any(|a| a.contains("virtio-gpu")));
    }

    #[test]
    fn gpu_display_args_for_virtio_gpu_has_no_gl_context() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::VirtioGpu;
        let args = gpu_display_args(&cfg).unwrap();
        assert!(args.contains(&"virtio-gpu-pci".to_string()));
        assert!(!args.iter().any(|a| a.contains("venus")));
    }

    #[test]
    fn gpu_display_args_for_none_display_engine_uses_plain_display_none() {
        let mut cfg = start_sh_equivalent_config();
        cfg.display.display_engine = DisplayEngine::None;
        let args = gpu_display_args(&cfg).unwrap();

        let display_idx = args
            .iter()
            .position(|a| a == "-display")
            .expect("-display must be present");
        assert_eq!(args[display_idx + 1], "none");
    }

    #[test]
    fn gpu_display_args_for_gtk_includes_native_clipboard() {
        let mut cfg = start_sh_equivalent_config();
        cfg.display.display_engine = DisplayEngine::Gtk;
        let args = gpu_display_args(&cfg).unwrap();
        let display_idx = args
            .iter()
            .position(|a| a == "-display")
            .expect("-display must be present");
        assert_eq!(
            args[display_idx + 1],
            "gtk,gl=on,show-cursor=off,clipboard=on,window-close=off"
        );
    }

    #[test]
    fn display_resolution_fwcfg_args_passes_resolution_string() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            display_resolution_fwcfg_args(&cfg),
            Some(vec![
                "-fw_cfg".to_string(),
                "name=opt/andler/display-resolution,string=1920x1080".to_string(),
            ])
        );
    }

    #[test]
    fn gpu_display_args_rejects_passthrough_as_not_implemented() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let err = gpu_display_args(&cfg).unwrap_err();
        assert!(matches!(err, BackendError::NotImplemented { .. }));
    }

    #[test]
    fn disk_args_match_start_sh_for_linux_vm_with_iso() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            disk_args(&cfg),
            vec![
                "-blockdev",
                "driver=file,node-name=file-disk0,filename=disk.qcow2,aio=threads,discard=unmap",
                "-blockdev",
                "driver=qcow2,node-name=drive-disk0,file=file-disk0,detect-zeroes=on",
                "-device",
                "virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4",
                "-drive",
                "file=cachyos-desktop-linux-260426.iso,media=cdrom,if=none,id=drive-cd0",
                "-device",
                "ide-cd,drive=drive-cd0,id=cd0,bootindex=2",
            ]
        );
    }

    #[test]
    fn disk_args_use_virtio_scsi_controller_and_scsi_cd_when_selected() {
        let mut cfg = start_sh_equivalent_config();
        cfg.kind = InstanceKind::LinuxVm {
            iso_path: PathBuf::from("cachyos-desktop-linux-260426.iso"),
            cdrom_bus: CdromBus::VirtioScsi,
        };
        assert_eq!(
            disk_args(&cfg),
            vec![
                "-blockdev",
                "driver=file,node-name=file-disk0,filename=disk.qcow2,aio=threads,discard=unmap",
                "-blockdev",
                "driver=qcow2,node-name=drive-disk0,file=file-disk0,detect-zeroes=on",
                "-device",
                "virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4",
                "-drive",
                "file=cachyos-desktop-linux-260426.iso,media=cdrom,if=none,id=drive-cd0",
                "-device",
                "virtio-scsi-pci,id=scsi0",
                "-device",
                "scsi-cd,drive=drive-cd0,bus=scsi0.0,id=cd0,bootindex=2",
            ]
        );
    }

    #[test]
    fn disk_args_have_no_cdrom_for_android_vm() {
        use andler_core::{AndroidProfile, AndroidVersion, ArmTranslator};

        let mut cfg = start_sh_equivalent_config();
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: true,
                microg: false,
                arm_translator: ArmTranslator::Libndk,
            },
        };
        let args = disk_args(&cfg);
        assert!(!args.iter().any(|a| a.contains("cdrom")));
        assert!(!args.iter().any(|a| a.contains("ide-cd")));
    }

    #[test]
    fn input_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            input_args(&cfg),
            vec![
                "-device",
                "virtio-tablet-pci,id=tablet0",
                "-device",
                "virtio-serial-pci",
                "-device",
                "virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0",
                "-chardev",
                "qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on",
            ]
        );
    }

    #[test]
    fn input_args_omit_clipboard_when_disabled() {
        let mut cfg = start_sh_equivalent_config();
        cfg.input.clipboard_enabled = false;
        let args = input_args(&cfg);
        assert!(!args.iter().any(|a| a.contains("vdagent")));
    }

    #[test]
    fn input_args_use_virtio_mouse_when_pointer_mode_is_mouse() {
        let mut cfg = start_sh_equivalent_config();
        cfg.input.pointer_mode = PointerMode::Mouse;
        let args = input_args(&cfg);
        assert!(args.contains(&"virtio-mouse-pci,id=mouse0".to_string()));
        assert!(!args.iter().any(|a| a.contains("virtio-tablet-pci")));
    }

    #[test]
    fn network_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            network_args(&cfg),
            vec!["-nic", "user,model=virtio-net-pci"]
        );
    }

    #[test]
    fn network_args_use_passt_netdev_when_selected() {
        let mut cfg = start_sh_equivalent_config();
        cfg.network.nat_backend = NatBackend::Passt;
        assert_eq!(
            network_args(&cfg),
            vec![
                "-netdev",
                "passt,id=net0",
                "-device",
                "virtio-net-pci,netdev=net0",
            ]
        );
    }
    #[test]
    fn network_args_bridge_mode_generates_correct_args() {
        let mut cfg = start_sh_equivalent_config();
        cfg.network.mode = NetworkMode::Bridge {
            interface: "br0".to_string(),
        };
        let args = network_args(&cfg);
        let tap_iface = primary_net_bridge_tap_iface(&cfg.id.to_string());
        assert_eq!(
            args,
            vec![
                "-netdev".to_string(),
                format!("tap,id=net0,ifname={tap_iface},bridge=br0,script=no,downscript=no"),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        );
    }

    #[test]
    fn primary_bridge_tap_iface_truncates_full_instance_id_below_ifnamsiz() {
        let full = "b".repeat(64);
        let name = primary_net_bridge_tap_iface(&full);
        assert_eq!(name, "tapbbbbbbbb");
        assert!(name.len() <= 15, "tap name must fit IFNAMSIZ: {name}");
    }

    #[test]
    fn network_args_isolated_mode_generates_correct_args() {
        let mut cfg = start_sh_equivalent_config();
        cfg.network.mode = NetworkMode::Isolated;
        let args = network_args(&cfg);
        assert_eq!(
            args,
            vec![
                "-netdev".to_string(),
                "tap,id=net0,ifname=andler0,script=no,downscript=no".to_string(),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        );
    }

    #[test]
    fn audio_args_match_reference_default_virtio_sound() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            audio_args(&cfg),
            vec![
                "-audiodev",
                "pipewire,id=snd0",
                "-device",
                "virtio-sound-pci,audiodev=snd0",
            ]
        );
    }

    #[test]
    fn audio_args_ich9_hda_matches_start_sh_literal() {
        let mut cfg = start_sh_equivalent_config();
        cfg.audio.device = AudioDevice::Ich9Hda;
        assert_eq!(
            audio_args(&cfg),
            vec![
                "-audiodev",
                "pipewire,id=snd0",
                "-device",
                "ich9-intel-hda",
                "-device",
                "hda-output,audiodev=snd0",
            ]
        );
    }

    #[test]
    fn audio_args_empty_when_backend_none() {
        let mut cfg = start_sh_equivalent_config();
        cfg.audio.backend = AudioBackend::None;
        assert_eq!(audio_args(&cfg), Vec::<String>::new());
    }

    #[test]
    fn qemu_size_suffix_prefers_gib_when_exact() {
        assert_eq!(qemu_size_suffix(8 * 1024 * 1024 * 1024), "8G");
    }

    #[test]
    fn qemu_size_suffix_falls_back_to_mib_when_not_gib_aligned() {
        assert_eq!(qemu_size_suffix(100 * 1024 * 1024), "100M");
    }

    #[test]
    fn build_args_ends_with_boot_menu() {
        let cfg = start_sh_equivalent_config();
        let qmp_path = PathBuf::from("/tmp/andler/linux/qmp.sock");
        let args = build_args(&cfg, &qmp_path).unwrap();
        assert_eq!(
            &args[args.len() - 2..],
            &["-boot".to_string(), "menu=on".to_string()]
        );
    }

    #[test]
    fn build_args_contains_all_blocks_for_start_sh_equivalent() {
        let cfg = start_sh_equivalent_config();
        let qmp_path = PathBuf::from("/tmp/andler/linux/qmp.sock");
        let args = build_args(&cfg, &qmp_path).unwrap();
        let joined = args.join(" ");
        for expected_fragment in [
            "q35,accel=kvm,usb=on",
            "host,kvm=on,+topoext,migratable=no",
            "memory-backend-memfd",
            "OVMF_CODE.4m.fd",
            "virtio-gpu-gl,hostmem=4G,blob=true,venus=true",
            "sdl,gl=on,show-cursor=off,window-close=off",
            "driver=file,node-name=file-disk0,filename=disk.qcow2,aio=threads,discard=unmap",
            "driver=qcow2,node-name=drive-disk0,file=file-disk0,detect-zeroes=on",
            "virtio-tablet-pci",
            "qemu-vdagent",
            "user,model=virtio-net-pci",
            "pipewire",
            "unix:/tmp/andler/linux/qmp.sock,server,nowait",
            "-serial",
            "chardev:console0",
            &format!(
                "console0,path={},server=on,wait=off",
                andler_core::paths::console_socket_path(&cfg.id).display()
            ),
        ] {
            assert!(
                joined.contains(expected_fragment),
                "expected build_args() output to contain `{expected_fragment}`, got: {joined}"
            );
        }
    }

    #[test]
    fn qmp_args_match_expected_format() {
        let path = PathBuf::from("/run/andler/instance-abc/qmp.sock");
        assert_eq!(
            qmp_args(&path),
            vec![
                "-qmp",
                "unix:/run/andler/instance-abc/qmp.sock,server,nowait",
            ]
        );
    }

    #[test]
    fn guest_agent_args_wire_agent_port_without_extra_bus_when_clipboard_on() {
        let path = PathBuf::from("/run/andler/instance-abc/qmp.sock");

        let mut cfg = start_sh_equivalent_config();
        cfg.input.clipboard_enabled = true;
        let args = guest_agent_args(&cfg, &path);
        assert_eq!(
            args,
            vec![
                "-chardev",
                "socket,id=qga,path=/run/andler/instance-abc/qmp.qga.sock,server=on,wait=off",
                "-device",
                "virtserialport,chardev=qga,id=qga,name=org.qemu.guest_agent.0",
            ]
        );

        cfg.input.clipboard_enabled = false;
        let args = guest_agent_args(&cfg, &path);
        assert_eq!(
            args,
            vec![
                "-chardev",
                "socket,id=qga,path=/run/andler/instance-abc/qmp.qga.sock,server=on,wait=off",
                "-device",
                "virtio-serial-pci",
                "-device",
                "virtserialport,chardev=qga,id=qga,name=org.qemu.guest_agent.0",
            ]
        );
    }

    #[test]
    fn serial_args_places_console_log_next_to_disk() {
        let mut cfg = start_sh_equivalent_config();
        cfg.disk.path = PathBuf::from("/home/user/.andler/instances/abc123/disk.qcow2");
        let args = serial_args(&cfg);
        assert_eq!(
            args,
            vec![
                "-chardev",
                &format!(
                    "socket,id=console0,path={},server=on,wait=off,logfile={},logappend=on",
                    andler_core::paths::console_socket_path(&cfg.id).display(),
                    "/home/user/.andler/instances/abc123/console.log"
                ),
                "-serial",
                "chardev:console0",
            ]
        );
    }

    #[test]
    fn extra_disks_appear_after_primary_without_bootindex() {
        let mut cfg = start_sh_equivalent_config();
        cfg.extra_disks.push(DiskConfig::standalone(
            PathBuf::from("/home/user/extra-data.qcow2"),
            16 * DiskConfig::GIB,
        ));
        let args = disk_args(&cfg);
        let start = args
            .windows(2)
            .position(|w| w[0] == "-drive" && w[1].contains("id=drive-extra0"))
            .expect("extra disk drive args present");
        assert_eq!(
            args[start..start + 4],
            vec![
                "-drive".to_string(),
                "file=/home/user/extra-data.qcow2,format=qcow2,if=none,id=drive-extra0,discard=on,detect-zeroes=on,aio=threads".to_string(),
                "-device".to_string(),
                "virtio-blk-pci,drive=drive-extra0,id=extra0,bus=root-port-0".to_string(),
            ]
        );
        let extra_args = args[start..start + 4].join(" ");
        assert!(
            !extra_args.contains("bootindex"),
            "extra disks must not claim a boot order slot: {extra_args}"
        );
        assert!(
            !extra_args.contains("num-queues"),
            "extra disks must not hardcode virtio queue counts: {extra_args}"
        );
    }

    #[test]
    fn extra_disk_ids_follow_index_scheme() {
        assert_eq!(extra_disk_drive_id(1), "drive-extra1");
        assert_eq!(extra_disk_device_id(1), "extra1");
    }

    #[test]
    fn extra_net_ids_and_host_ifaces_follow_index_scheme() {
        assert_eq!(extra_net_id(2), "net-extra2");
        assert_eq!(extra_net_bridge_tap_iface("deadbeef", 2), "tapdeadbeef-e2");
        assert_eq!(extra_net_isolated_iface(2), "andler-e2");
    }

    #[test]
    fn bridge_tap_iface_truncates_full_instance_id_below_ifnamsiz() {
        let full = "a".repeat(64);
        let name = extra_net_bridge_tap_iface(&full, 9);
        assert_eq!(name, "tapaaaaaaaa-e9");
        assert!(name.len() <= 15, "tap name must fit IFNAMSIZ: {name}");
    }

    #[test]
    fn extra_networks_append_after_primary_for_each_mode() {
        let mut cfg = start_sh_equivalent_config();
        cfg.extra_networks.push(NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Slirp,
        });
        cfg.extra_networks.push(NetworkConfig {
            mode: NetworkMode::Bridge {
                interface: "br0".to_string(),
            },
            device_model: "e1000e".to_string(),
            nat_backend: NatBackend::Slirp,
        });
        cfg.extra_networks.push(NetworkConfig {
            mode: NetworkMode::Isolated,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Slirp,
        });

        let args = network_args(&cfg);
        let primary_len = primary_network_args(&cfg).len();
        assert_eq!(
            args[primary_len..],
            vec![
                "-netdev".to_string(),
                "user,id=net-extra0".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net-extra0,bus=root-port-8".to_string(),
                "-netdev".to_string(),
                format!(
                    "tap,id=net-extra1,ifname={},bridge=br0,script=no,downscript=no",
                    extra_net_bridge_tap_iface(&cfg.id.to_string(), 1)
                ),
                "-device".to_string(),
                "e1000e,netdev=net-extra1,bus=root-port-9".to_string(),
                "-netdev".to_string(),
                "tap,id=net-extra2,ifname=andler-e2,script=no,downscript=no".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net-extra2,bus=root-port-10".to_string(),
            ]
        );
    }

    #[test]
    fn extra_network_uses_passt_netdev_when_selected() {
        let mut cfg = start_sh_equivalent_config();
        cfg.extra_networks.push(NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Passt,
        });
        let args = network_args(&cfg);
        assert!(args.contains(&"passt,id=net-extra0".to_string()));
    }
}
