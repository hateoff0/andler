

use std::path::Path;

use andler_core::{
    AudioBackend, AudioDevice, CdromBus, DiskFormat, DisplayEngine, InstanceConfig, InstanceKind,
    NatBackend, PointerMode, RenderBackend,
};


pub fn build_args(cfg: &InstanceConfig, qmp_socket_path: &Path) -> Vec<String> {
    let mut args = Vec::new();
    args.extend(name_args(cfg));
    args.extend(machine_and_cpu_args(cfg));
    args.extend(memory_args(cfg));
    args.extend(firmware_args(cfg));
    args.extend(gpu_display_args(cfg));
    args.extend(disk_args(cfg));
    args.extend(input_args(cfg));
    args.extend(network_args(cfg));
    args.extend(audio_args(cfg));
    args.extend(qmp_args(qmp_socket_path));
    args.push("-boot".to_string());
    args.push("menu=on".to_string());
    args
}


fn name_args(cfg: &InstanceConfig) -> Vec<String> {
    vec!["-name".to_string(), format!("{},process={}", cfg.name, cfg.name)]
}


fn qmp_args(qmp_socket_path: &Path) -> Vec<String> {
    vec![
        "-qmp".to_string(),
        format!("unix:{},server,nowait", qmp_socket_path.display()),
    ]
}


fn machine_and_cpu_args(cfg: &InstanceConfig) -> Vec<String> {
    let cpu = &cfg.cpu;
    vec![
        "-machine".to_string(),
        "q35,accel=kvm,usb=on".to_string(),
        "-cpu".to_string(),
        "host,kvm=on,+topoext,migratable=no".to_string(),
        "-smp".to_string(),
        format!(
            "cpus={},sockets={},dies=1,cores={},threads={}",
            cpu.cores, cpu.sockets, cpu.cores, cpu.threads
        ),
    ]
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


fn gpu_display_args(cfg: &InstanceConfig) -> Vec<String> {
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
            panic!(
                "RenderBackend::Passthrough reached andler-qemu::cmdline::build_args; \
                 caller must reject it via RenderBackend::is_implemented() before this point"
            );
        }
    }

    let show_cursor = if cfg.input.hide_host_cursor { "off" } else { "on" };
    let display_str = match cfg.display.display_engine {
        DisplayEngine::Sdl => format!(
            "sdl,gl={},show-cursor={}",
            if gpu.gl { "on" } else { "off" },
            show_cursor
        ),
        DisplayEngine::Gtk => format!(
            "gtk,gl={},show-cursor={},clipboard=on",
            if gpu.gl { "on" } else { "off" },
            show_cursor
        ),
        DisplayEngine::Spice => "spice-app".to_string(),
        DisplayEngine::Dbus => "dbus".to_string(),
        DisplayEngine::None => "none".to_string(),
    };
    args.push("-display".to_string());
    args.push(display_str);

    args
}


fn disk_args(cfg: &InstanceConfig) -> Vec<String> {
    let disk = &cfg.disk;
    let format_str = match disk.format {
        DiskFormat::Qcow2 => "qcow2",
        DiskFormat::Raw => "raw",
        DiskFormat::Vdi => "vdi",
    };

    let mut args = vec![
        "-drive".to_string(),
        format!(
            "file={},format={},if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads",
            disk.path.display(),
            format_str
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4".to_string(),
    ];

    if let InstanceKind::LinuxVm { iso_path, cdrom_bus } = &cfg.kind {
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
                args.push(
                    "scsi-cd,drive=drive-cd0,bus=scsi0.0,id=cd0,bootindex=2".to_string(),
                );
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
    use andler_core::NetworkMode;

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
            let tap_iface = format!("tap{}", cfg.id.0);
            vec![
                "-netdev".to_string(),
                format!("tap,id=net0,ifname={},bridge={},script=no,downscript=no", tap_iface, bridge),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        },
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
            let mut args = vec![
                "-audiodev".to_string(),
                format!("{backend_str},id=snd0"),
            ];
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

    if bytes % GIB == 0 {
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
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
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
            gpu_display_args(&cfg),
            vec![
                "-vga",
                "none",
                "-device",
                "virtio-gpu-gl,hostmem=4G,blob=true,venus=true",
                "-display",
                "sdl,gl=on,show-cursor=off",
            ]
        );
    }

    #[test]
    fn gpu_display_args_for_cpu_backend_uses_vga_std() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::Cpu;
        let args = gpu_display_args(&cfg);
        assert_eq!(&args[0..2], &["-vga".to_string(), "std".to_string()]);
        assert!(!args.iter().any(|a| a.contains("virtio-gpu")));
    }

    #[test]
    fn gpu_display_args_for_virtio_gpu_has_no_gl_context() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::VirtioGpu;
        let args = gpu_display_args(&cfg);
        assert!(args.contains(&"virtio-gpu-pci".to_string()));
        assert!(!args.iter().any(|a| a.contains("venus")));
    }

    #[test]
    fn gpu_display_args_for_none_display_engine_uses_plain_display_none() {
        let mut cfg = start_sh_equivalent_config();
        cfg.display.display_engine = DisplayEngine::None;
        let args = gpu_display_args(&cfg);

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
        let args = gpu_display_args(&cfg);
        let display_idx = args
            .iter()
            .position(|a| a == "-display")
            .expect("-display must be present");
        assert_eq!(args[display_idx + 1], "gtk,gl=on,show-cursor=off,clipboard=on");
    }

    #[test]
    #[should_panic(expected = "Passthrough")]
    fn gpu_display_args_panics_on_passthrough() {
        let mut cfg = start_sh_equivalent_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let _ = gpu_display_args(&cfg);
    }

    #[test]
    fn disk_args_match_start_sh_for_linux_vm_with_iso() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(
            disk_args(&cfg),
            vec![
                "-drive",
                "file=disk.qcow2,format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads",
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
                "-drive",
                "file=disk.qcow2,format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads",
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
        assert_eq!(network_args(&cfg), vec!["-nic", "user,model=virtio-net-pci"]);
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
        assert_eq!(
            args,
            vec![
                "-netdev".to_string(),
                format!("tap,id=net0,ifname=tap{},bridge=br0,script=no,downscript=no", cfg.id.0),
                "-device".to_string(),
                format!("{},netdev=net0", cfg.network.device_model),
            ]
        );
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
        let args = build_args(&cfg, &qmp_path);
        assert_eq!(&args[args.len() - 2..], &["-boot".to_string(), "menu=on".to_string()]);
    }

    #[test]
    fn build_args_contains_all_blocks_for_start_sh_equivalent() {
        let cfg = start_sh_equivalent_config();
        let qmp_path = PathBuf::from("/tmp/andler/linux/qmp.sock");
        let args = build_args(&cfg, &qmp_path);
        let joined = args.join(" ");
        for expected_fragment in [
            "q35,accel=kvm,usb=on",
            "host,kvm=on,+topoext,migratable=no",
            "memory-backend-memfd",
            "OVMF_CODE.4m.fd",
            "virtio-gpu-gl,hostmem=4G,blob=true,venus=true",
            "sdl,gl=on,show-cursor=off",
            "discard=on,detect-zeroes=on,aio=threads",
            "virtio-tablet-pci",
            "qemu-vdagent",
            "user,model=virtio-net-pci",
            "pipewire",
            "unix:/tmp/andler/linux/qmp.sock,server,nowait",
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
}
