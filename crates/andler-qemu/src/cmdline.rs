//! Сборка аргументов командной строки QEMU из `InstanceConfig`.
//!
//! Источник истины — `scripts/start.sh` (см.
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.4). Каждая функция здесь
//! соответствует одному логическому блоку флагов из этого скрипта и
//! тестируется отдельно сравнением с ожидаемым результатом для
//! `reference_default()`-конфигураций — так расхождение с `start.sh` в
//! любом отдельном блоке (CPU, GPU, диск, ...) обнаруживается локализованно,
//! а не "что-то не так со всей командной строкой".
//!
//! Все функции — чистые: `InstanceConfig -> Vec<String>`, без обращения к
//! файловой системе или запуска процессов. Эта чистота — то, что позволяет
//! тестировать `cmdline.rs` без `/dev/kvm` (см. README этого крейта).

use std::path::Path;

use andler_core::{
    AudioBackend, DiskFormat, DisplayEngine, InstanceConfig, InstanceKind, RenderBackend,
};

/// Собирает полный список аргументов для `qemu-system-x86_64`, эквивалентный
/// тому, что строит `exec qemu-system-x86_64 ...` в `start.sh`, но из
/// декларативного `InstanceConfig` вместо фиксированных переменных скрипта.
///
/// `qmp_socket_path` — путь к unix-сокету QMP, на котором `process.rs`
/// должен поднять `-qmp unix:<path>,server,nowait`. Не часть `InstanceConfig`
/// — это деталь конкретного запуска, которую решает `andler-qemu`/
/// `andler-daemon` (обычно на основе `InstanceId`), а не декларативная
/// конфигурация инстанса; `start.sh` не содержал этого флага явно, так как
/// был написан для интерактивного запуска человеком, а не для
/// программного управления через `process.rs`/`qmp.rs`.
///
/// Порядок блоков аргументов сохранён как в `start.sh`, хотя для самого
/// QEMU порядок большинства флагов не важен — сохранение порядка облегчает
/// построчное сравнение результата с референсным скриптом при отладке.
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

/// `-name <name>,process=<name>`.
fn name_args(cfg: &InstanceConfig) -> Vec<String> {
    vec!["-name".to_string(), format!("{},process={}", cfg.name, cfg.name)]
}

/// `-qmp unix:<path>,server,nowait` — QMP-сокет, на котором `andler-qemu`
/// слушает команды управления (pause/resume/snapshot/`query-*`, см.
/// `qmp.rs`, следующий шаг реализации). `server,nowait` — QEMU слушает
/// сокет и не блокирует запуск, ожидая подключения клиента; клиент
/// (`qmp.rs`) подключается уже после старта процесса.
fn qmp_args(qmp_socket_path: &Path) -> Vec<String> {
    vec![
        "-qmp".to_string(),
        format!("unix:{},server,nowait", qmp_socket_path.display()),
    ]
}

/// `-machine q35,accel=kvm,usb=on` + `-cpu host,kvm=on,+topoext,migratable=no`
/// + `-smp cpus=N,sockets=N,dies=1,cores=N,threads=N`.
///
/// `dies=1` зафиксировано как константа: `CpuConfig` (см.
/// `andler-core::config::cpu`) не заводит поле `dies`, так как ни план, ни
/// `start.sh` не предусматривают множественные dies — это деталь топологии
/// QEMU без соответствующего домена в нашей модели на этом этапе.
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

/// `-m <size>` + `-object memory-backend-memfd,id=mem1,size=<size>,share=on`
/// + `-machine memory-backend=mem1`.
///
/// `share=on` запрашивается всегда, когда включён `ksm` в `MemoryConfig` —
/// shared-память — это именно то, что делает страницы доступными для
/// объединения KSM на хосте (см. docs/architecture/CORE_ARCHITECTURE_PLAN.md,
/// §6.1.1). Если `ksm = false`, memfd-backend всё равно используется (как в
/// `start.sh`), но без `share=on` — обычная private-память процесса.
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

/// `-drive if=pflash,format=raw,readonly=on,file=<OVMF_CODE>` +
/// `-drive if=pflash,format=raw,file=<OVMF_VARS>`.
fn firmware_args(cfg: &InstanceConfig) -> Vec<String> {
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

/// `-vga none` + GPU-устройство (зависит от `RenderBackend`) + `-display ...`.
///
/// Соответствие `RenderBackend` -> флаги устройства:
/// - `Venus` -> `virtio-gpu-gl,hostmem=...,blob=...,venus=true` (как в `start.sh`)
/// - `VirGl` -> `virtio-gpu-gl,hostmem=...,blob=...` без `venus=true` —
///   тот же virtio-gpu-gl device, но без Vulkan-контекста, что в терминах
///   QEMU и есть обычный VirGL/OpenGL-рендеринг
/// - `VirtioGpu` -> `virtio-gpu-pci` без gl-контекста — самый совместимый
///   аппаратно-ускоренный вариант без host GL/Vulkan passthrough
/// - `Cpu` -> без virtio-gpu устройства вообще; `-vga std` вместо `-vga none`
/// - `Passthrough` -> не должно достигать этой функции вообще: вызывающая
///   сторона (`andler-qemu::process` / `andler-daemon`) обязана проверить
///   `RenderBackend::is_implemented()` и вернуть `BackendError::InvalidConfig`
///   до вызова `build_args`. Здесь это явный `panic!`, а не молчаливая
///   подстановка несуществующих флагов — наличие непокрытого варианта на
///   этом этапе означает ошибку выше по стеку вызовов, а не штатный путь.
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
                 caller must reject it via RenderBackend::is_implemented() before this point \
                 (see docs/architecture/CORE_ARCHITECTURE_PLAN.md, §2.3)"
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
        // Spice/Dbus — нужны для стриминга в GUI-клиент (frontend/), не
        // используются текущим CLI-путём. Конкретные флаги (порт, TLS,
        // и т.п.) — открытый вопрос на момент реализации GUI, не этого шага.
        DisplayEngine::Spice => "spice-app".to_string(),
        DisplayEngine::Dbus => "dbus".to_string(),
    };
    args.push("-display".to_string());
    args.push(display_str);

    args
}

/// Аргументы диска(ов) инстанса.
///
/// `-drive file=...,format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads`
/// + `-device virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4`.
///
/// Для `InstanceKind::LinuxVm { iso_path }` дополнительно добавляется
/// CD-ROM: `-drive file=<iso>,media=cdrom,if=none,id=drive-cd0` +
/// `-device ide-cd,drive=drive-cd0,id=cd0,bootindex=2` — соответствует
/// установочному ISO в `start.sh`. Для `InstanceKind::AndroidVm` CD-ROM не
/// добавляется: гостевой образ уже содержит готовую систему с Waydroid
/// (см. §4.4 архитектурного плана), установочного носителя не требуется.
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

    if let InstanceKind::LinuxVm { iso_path } = &cfg.kind {
        args.push("-drive".to_string());
        args.push(format!(
            "file={},media=cdrom,if=none,id=drive-cd0",
            iso_path.display()
        ));
        args.push("-device".to_string());
        args.push("ide-cd,drive=drive-cd0,id=cd0,bootindex=2".to_string());
    }

    args
}

/// `-device virtio-tablet-pci,id=tablet0` (если `tablet_mode`) +
/// `-device virtio-serial-pci` + `-device virtserialport,...` +
/// `-chardev qemu-vdagent,...,clipboard=on,mouse=on` (если `clipboard_enabled`).
fn input_args(cfg: &InstanceConfig) -> Vec<String> {
    let input = &cfg.input;
    let mut args = Vec::new();

    if input.tablet_mode {
        args.push("-device".to_string());
        args.push("virtio-tablet-pci,id=tablet0".to_string());
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

/// `-nic user,model=virtio-net-pci` (NAT) или эквивалент для других режимов.
///
/// `Bridge`/`Isolated` пока не реализованы в `andler-net` (см. его README)
/// — здесь оставлен `panic!` для непокрытых вариантов по той же причине,
/// что и для `RenderBackend::Passthrough` в `gpu_display_args`: молчаливая
/// подмена на NAT была бы тихим расхождением между запрошенной и реальной
/// конфигурацией сети, что хуже явного отказа.
fn network_args(cfg: &InstanceConfig) -> Vec<String> {
    use andler_core::NetworkMode;

    match &cfg.network.mode {
        NetworkMode::Nat => vec![
            "-nic".to_string(),
            format!("user,model={}", cfg.network.device_model),
        ],
        NetworkMode::Bridge { .. } | NetworkMode::Isolated => {
            panic!(
                "NetworkMode::{:?} is not yet implemented in andler-qemu::cmdline \
                 (see crates/andler-net/README.md)",
                cfg.network.mode
            );
        }
    }
}

/// `-audiodev pipewire,id=snd0 -device ich9-intel-hda -device hda-output,audiodev=snd0`
/// (или `pulseaudio` вместо `pipewire`; никаких audio-флагов для `None`).
fn audio_args(cfg: &InstanceConfig) -> Vec<String> {
    match cfg.audio.backend {
        AudioBackend::None => Vec::new(),
        AudioBackend::Pipewire | AudioBackend::Pulseaudio => {
            let backend_str = match cfg.audio.backend {
                AudioBackend::Pipewire => "pipewire",
                AudioBackend::Pulseaudio => "pulseaudio",
                AudioBackend::None => unreachable!(),
            };
            vec![
                "-audiodev".to_string(),
                format!("{backend_str},id=snd0"),
                "-device".to_string(),
                "ich9-intel-hda".to_string(),
                "-device".to_string(),
                "hda-output,audiodev=snd0".to_string(),
            ]
        }
    }
}

/// Переводит байты в строку вида `"8G"`/`"4096M"`, которую понимает QEMU
/// в флагах размера памяти (`-m`, `hostmem=...`, `size=...`).
///
/// Предпочитает `G`, если число кратно гигабайту, иначе `M` — это
/// соответствует тому, как параметры заданы в `start.sh` (`8G` для RAM,
/// `4096M` для VRAM, хотя `4096M` тоже кратно гигабайту: `start.sh` просто
/// использует `M` для VRAM по соглашению скрипта, а не из необходимости).
/// Эта функция предпочитает `G` всегда, когда возможно — расхождение в
/// форме (`4G` вместо `4096M`) не влияет на поведение QEMU, оба варианта
/// эквивалентны для него.
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
        InputConfig, InstanceConfig, InstanceId, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    /// Конфигурация `InstanceKind::LinuxVm`, дословно соответствующая
    /// `start.sh` (имя `linux`, ISO `cachyos-desktop-linux-260426.iso`,
    /// все остальные `reference_default()`).
    fn start_sh_equivalent_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "linux".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("cachyos-desktop-linux-260426.iso"),
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
                "if=pflash,format=raw,readonly=on,file=/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd",
                "-drive",
                "if=pflash,format=raw,file=linux_VARS.fd",
            ]
        );
    }

    #[test]
    fn gpu_display_args_match_start_sh_for_venus() {
        let cfg = start_sh_equivalent_config();
        // hostmem в start.sh записан как "4096M", но qemu_size_suffix
        // нормализует кратные гигабайту значения в форму "G" (см. её
        // документацию) — "4G" и "4096M" эквивалентны для QEMU, отличается
        // только текстовая форма, не поведение.
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
    fn disk_args_have_no_cdrom_for_android_vm() {
        use andler_core::{AndroidProfile, AndroidVersion, RootMode};

        let mut cfg = start_sh_equivalent_config();
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: true,
                microg: false,
                libndk: true,
                root: RootMode::None,
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
    fn network_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
        assert_eq!(network_args(&cfg), vec!["-nic", "user,model=virtio-net-pci"]);
    }

    #[test]
    fn audio_args_match_start_sh() {
        let cfg = start_sh_equivalent_config();
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
        // 4096M (start.sh hostmem) на самом деле кратно гигабайту и вернёт
        // "4G" — это проверяется отдельно в gpu_display_args_match_start_sh_for_venus.
        // Здесь — намеренно не кратное гигабайту значение, чтобы проверить
        // именно MiB-фоллбэк.
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
