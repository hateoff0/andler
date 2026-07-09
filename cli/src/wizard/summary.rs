//! Summary screen and confirmation before VM creation.

use andler_core::{
    AudioBackend, CdromBus, DisplayEngine, NatBackend, PointerMode, RenderBackend,
    Resolution,
};
use andler_firmware::HardwareDefaults;
use inquire::Select;

use crate::{CliArmTranslator, CliRootMode};

use super::advanced::AdvancedConfig;
use super::basic::BasicResult;
use super::{map_inquire_err, WizardError, WizardKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryAction {
    Create,
    Modify,
    Cancel,
}

pub fn run(
    basic: &BasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> Result<SummaryAction, WizardError> {
    print_summary(basic, advanced, detected);

    let choice = Select::new(
        "Proceed?",
        vec!["Create VM", "Modify advanced settings", "Cancel"],
    )
    .prompt()
    .map_err(map_inquire_err)?;

    Ok(match choice {
        "Create VM" => SummaryAction::Create,
        "Modify advanced settings" => SummaryAction::Modify,
        _ => SummaryAction::Cancel,
    })
}

fn print_summary(
    basic: &BasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) {
    let is_basic = advanced.is_none();
    let suffix = |explicit: bool| {
        if is_basic && !explicit {
            " (default)"
        } else if !explicit && is_basic {
            " (auto-detected)"
        } else {
            ""
        }
    };

    let compact = advanced.map(|a| a.compact_on_shutdown).unwrap_or(false);
    let gpu_render = advanced
        .map(|a| a.gpu_render.clone())
        .unwrap_or_else(|| detected.gpu_render.clone());
    let gpu_memory_mib = advanced.map(|a| a.gpu_memory_mib).unwrap_or(4096);
    let resolution = advanced
        .map(|a| a.display_resolution)
        .unwrap_or_else(|| Resolution::new(1920, 1080));
    let fullscreen = advanced.map(|a| a.fullscreen).unwrap_or(false);
    let audio = advanced
        .map(|a| a.audio_backend)
        .unwrap_or(detected.audio_server);
    let clipboard = advanced.map(|a| a.clipboard_enabled).unwrap_or(true);
    let pointer = advanced
        .map(|a| a.input_pointer)
        .unwrap_or(PointerMode::Tablet);
    let cpu_cores = advanced.map(|a| a.cpu_cores).unwrap_or(4);
    let memory_gib = advanced.map(|a| a.memory_gib).unwrap_or(8);

    let display_engine = if gpu_render == RenderBackend::Cpu {
        DisplayEngine::None
    } else {
        detected.display_engine
    };

    let nat_label = if detected.passt_available {
        "NAT/passt"
    } else {
        "NAT"
    };

    println!();
    println!("┌─ Summary before creation ─────────────────────────────────────┐");

    match basic {
        BasicResult::Linux(l) => {
            println!("│  Type:              Linux VM");
            println!("│  Name:              {}", l.name);
            if l.iso_path.is_empty() {
                println!("│  ISO:               (no ISO — boot from disk)");
            } else {
                println!("│  ISO:               {}", l.iso_path);
                let cdrom = advanced
                    .and_then(|a| a.cdrom_bus)
                    .unwrap_or_else(|| {
                        CdromBus::recommended_for_iso_filename(std::path::Path::new(&l.iso_path))
                    });
                println!("│  CD-ROM bus:        {cdrom:?}{}", suffix(advanced.is_some()));
            }
            let disk_name = format!("{}-disk.qcow2", l.name);
            println!(
                "│  Disk:              {}/<id>/{disk_name} ({} GiB, qcow2)",
                l.instances_root, l.disk_size_gib
            );
        }
        BasicResult::Android(a) => {
            println!("│  Type:              Android VM");
            println!("│  Name:              {}", a.name);
            println!("│  Base image:        {}", a.base_image);
            println!(
                "│  Android version:   {:?}{}",
                a.android_version,
                if is_basic { " (recommended)" } else { "" }
            );
            let disk_name = format!("{}-disk.qcow2", a.name);
            println!(
                "│  Disk:              {}/<id>/{disk_name} ({} GiB, qcow2)",
                a.instances_root, a.disk_size_gib
            );

            let arm = advanced
                .and_then(|adv| adv.arm_translator)
                .or_else(|| {
                    detected.arm_translator.map(|t| match t {
                        andler_core::ArmTranslator::Libndk => CliArmTranslator::Libndk,
                        andler_core::ArmTranslator::Libhoudini => CliArmTranslator::Libhoudini,
                        andler_core::ArmTranslator::None => CliArmTranslator::None,
                    })
                })
                .unwrap_or(CliArmTranslator::None);
            let gapps = advanced.map(|adv| adv.gapps).unwrap_or(false);
            let microg = advanced.map(|adv| adv.microg).unwrap_or(false);
            let root = advanced
                .and_then(|adv| adv.root_mode.as_ref())
                .map(|(m, d)| {
                    if *m == CliRootMode::Magisk {
                        format!("magisk ({d})")
                    } else {
                        "none".to_string()
                    }
                })
                .unwrap_or_else(|| "none".to_string());

            println!(
                "│  ARM translator:    {arm:?}{}",
                if is_basic && detected.arm_translator.is_some() {
                    " (auto-detected)"
                } else {
                    suffix(advanced.is_some())
                }
            );
            println!("│  GApps:             {gapps}{}", suffix(advanced.is_some()));
            println!("│  MicroG:            {microg}{}", suffix(advanced.is_some()));
            println!("│  Root:              {root}{}", suffix(advanced.is_some()));
        }
    }

    println!(
        "│  Compact on shutdown: {compact}{}",
        suffix(advanced.is_some())
    );

    match &detected.ovmf {
        Ok(found) => {
            println!(
                "│  OVMF VARS:         {}{}",
                found.vars_template.display(),
                if is_basic { " (auto-detected)" } else { "" }
            );
        }
        Err(_) if basic.kind() == WizardKind::Linux => {
            println!("│  OVMF VARS:         not found (Legacy BIOS will be used)");
        }
        Err(_) => {}
    }

    let gpu_suffix = if is_basic {
        " (auto-detected)"
    } else {
        ""
    };
    println!(
        "│  GPU:               {gpu_render:?}, {gpu_memory_mib} MiB{gpu_suffix}"
    );
    let display_mode = if fullscreen { "fullscreen" } else { "windowed" };
    println!(
        "│  Display:           {}x{}, {display_mode}{}",
        resolution.width,
        resolution.height,
        suffix(advanced.is_some())
    );
    let _ = display_engine;
    println!(
        "│  Audio:             {audio:?}{}",
        if is_basic {
            " (auto-detected)"
        } else {
            ""
        }
    );
    println!(
        "│  Clipboard:         {}{}",
        if clipboard { "enabled" } else { "disabled" },
        suffix(advanced.is_some())
    );
    println!(
        "│  Input pointer:     {pointer:?}{}",
        suffix(advanced.is_some())
    );
    println!(
        "│  Network:           {nat_label}{}",
        if is_basic { " (default)" } else { "" }
    );
    println!(
        "│  CPU:               {cpu_cores} cores{}",
        suffix(advanced.is_some())
    );
    println!(
        "│  Memory:            {memory_gib} GiB{}",
        suffix(advanced.is_some())
    );
    println!("└───────────────────────────────────────────────────────────────┘");
    println!();

    // qcow2 is thin-provisioned/sparse — a freshly created disk actually
    // takes up only a small fraction of its configured maximum size on
    // the host, growing as the guest writes data. Not a specific real
    // number (e.g. the plan's own "~2 GiB" mockup) — that depends on
    // qcow2 cluster size/version and isn't something worth pretending to
    // predict precisely; the point is just that the configured size is
    // a ceiling, not the actual disk usage. See PLAN.md, item 19,
    // "19c. Show estimated disk usage".
    println!(
        "Note: the {} GiB disk is thin-provisioned (qcow2) — it starts out small \
         (well under 1 GiB) and grows on demand as data is written, up to that size.",
        basic.disk_size_gib()
    );
    println!();

    // Clipboard sharing needs `spice-vdagentd` running *inside the
    // guest* — the host-side QEMU config above (`qemu-vdagent` chardev)
    // is correct on its own and does nothing without it. This is a
    // guest-side package the wizard/daemon has no way to install or
    // detect from the host, so the best we can do is tell the person
    // up front rather than let them discover a "broken" clipboard later
    // with no indication of why. See PLAN.md, item 3, "Clipboard
    // sharing does not work".
    if clipboard {
        println!("Note: clipboard sharing requires spice-vdagent running inside the guest OS.");
        println!("Install it after first boot:");
        println!("  Arch/CachyOS:    sudo pacman -S spice-vdagent");
        println!("  Ubuntu/Debian:   sudo apt install spice-vdagent");
        println!("  Fedora:          sudo dnf install spice-vdagent");
        println!();
    }
}

#[allow(dead_code)]
pub(crate) fn format_audio(backend: AudioBackend) -> &'static str {
    match backend {
        AudioBackend::Pipewire => "PipeWire",
        AudioBackend::Pulseaudio => "PulseAudio",
        AudioBackend::None => "None",
    }
}

pub(crate) fn format_nat_backend(passt_available: bool) -> NatBackend {
    if passt_available {
        NatBackend::Passt
    } else {
        NatBackend::Slirp
    }
}
