use andler_core::{DisplayEngine, NatBackend, NetworkMode, PointerMode, RenderBackend, Resolution};
use andler_firmware::HardwareDefaults;
use inquire::Select;

use crate::CliArmTranslator;

use super::advanced::AdvancedConfig;
use super::basic::BasicResult;
use super::build::resolve_arm_translator;
use super::ui::Panel;
use super::{map_inquire_err, ui, WizardError};

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
    print!("{}", summary_text(basic, advanced, detected));
    println!();

    let create = "Create the VM";
    let modify = "Change some settings";
    let cancel = "Cancel";
    let choice = Select::new("Proceed?", vec![create, modify, cancel])
        .with_help_message(
            "The guest-side selections below are installed inside the VM right after creation.",
        )
        .prompt()
        .map_err(map_inquire_err)?;

    Ok(if choice == create {
        SummaryAction::Create
    } else if choice == modify {
        SummaryAction::Modify
    } else {
        SummaryAction::Cancel
    })
}

/// The whole summary screen as text: rendered through `Panel` so its layout
/// is testable, and printed as one block by `run`.
fn summary_text(
    basic: &BasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
) -> String {
    let is_basic = advanced.is_none();
    let origin = |explicit: bool| {
        if explicit {
            String::new()
        } else if is_basic {
            " (default)".to_string()
        } else {
            " (auto-detected)".to_string()
        }
    };

    let gpu_render = advanced
        .map(|a| a.gpu_render.clone())
        .unwrap_or_else(|| detected.gpu_render.clone());
    let display_engine = if gpu_render == RenderBackend::Cpu {
        DisplayEngine::None
    } else {
        detected.display_engine
    };

    let mut panel = Panel::new("Summary before creation");
    panel.section(match basic {
        BasicResult::Linux(_) => "Linux VM",
        BasicResult::Android(_) => "Android VM",
    });
    panel.field("Name", basic.name());
    match basic {
        BasicResult::Linux(l) => {
            panel.field(
                "ISO",
                if l.iso_path.is_empty() {
                    "(none — boot from the disk)".to_string()
                } else {
                    l.iso_path.clone()
                },
            );
            if !l.iso_path.is_empty() {
                let bus = advanced.and_then(|a| a.cdrom_bus).unwrap_or_else(|| {
                    andler_core::CdromBus::recommended_for_iso_filename(std::path::Path::new(
                        &l.iso_path,
                    ))
                });
                panel.field(
                    "CD-ROM bus",
                    format!("{bus:?}{}", origin(advanced.is_some())),
                );
            }
        }
        BasicResult::Android(a) => {
            panel.field("Base image", a.base_image.clone());
            panel.field(
                "Android version",
                format!(
                    "Android {}{}",
                    a.android_version.number(),
                    if is_basic { " (recommended)" } else { "" }
                ),
            );
            let arm = resolve_arm_translator(advanced, detected);
            panel.field(
                "ARM translator",
                format!(
                    "{arm}{}",
                    if is_basic && detected.arm_translator.is_some() {
                        " (auto-detected)".to_string()
                    } else {
                        origin(advanced.is_some())
                    }
                ),
            );
            let gapps = advanced.map(|adv| adv.gapps).unwrap_or(a.gapps);
            panel.field("GApps", format!("{gapps}"));
            let linked_overlay = advanced.map(|adv| adv.linked_overlay).unwrap_or(false);
            panel.field(
                "Disk mode",
                format!(
                    "{}{}",
                    if linked_overlay {
                        "linked overlay (backed by the base image)"
                    } else {
                        "full copy (independent of the base image)"
                    },
                    origin(advanced.is_some())
                ),
            );
        }
    }
    panel.field(
        "Disk",
        format!(
            "{}/<id>/disk.qcow2 ({} GiB, thin-provisioned)",
            basic.instances_root(),
            basic.disk_size_gib()
        ),
    );
    panel.field(
        "Firmware",
        match basic {
            BasicResult::Linux(l) if !l.enable_uefi => "Legacy BIOS".to_string(),
            _ => match &detected.ovmf {
                Ok(found) => format!(
                    "UEFI/OVMF ({}){}",
                    found.vars_template.display(),
                    if is_basic { " (auto-detected)" } else { "" }
                ),
                Err(_) => "UEFI/OVMF (template not found)".to_string(),
            },
        },
    );

    panel.section("Display & devices");
    panel.field(
        "GPU",
        format!(
            "{}, {} MiB{}",
            ui::render_label(&gpu_render),
            advanced.map(|a| a.gpu_memory_mib).unwrap_or(4096),
            origin(advanced.is_some())
        ),
    );
    let resolution = advanced
        .map(|a| a.display_resolution)
        .unwrap_or_else(|| Resolution::new(1920, 1080));
    panel.field(
        "Display",
        format!(
            "{}x{}, {}{}",
            resolution.width,
            resolution.height,
            if advanced.map(|a| a.fullscreen).unwrap_or(false) {
                "fullscreen"
            } else {
                "windowed"
            },
            if matches!(display_engine, DisplayEngine::None) {
                " (headless: software rendering)"
            } else {
                ""
            }
        ),
    );
    panel.field(
        "Audio",
        format!(
            "{}{}",
            ui::audio_label(
                advanced
                    .map(|a| a.audio_backend)
                    .unwrap_or(detected.audio_server)
            ),
            origin(advanced.is_some())
        ),
    );
    let clipboard = advanced.map(|a| a.clipboard_enabled).unwrap_or(true);
    panel.field(
        "Clipboard",
        format!(
            "{}{}",
            if clipboard { "enabled" } else { "disabled" },
            origin(advanced.is_some())
        ),
    );
    panel.field(
        "Input pointer",
        format!(
            "{:?}{}",
            advanced
                .map(|a| a.input_pointer)
                .unwrap_or(PointerMode::Tablet),
            origin(advanced.is_some())
        ),
    );

    panel.section("CPU & network");
    panel.field(
        "CPU",
        format!(
            "{} cores{}",
            advanced.map(|a| a.cpu_cores).unwrap_or(4),
            origin(advanced.is_some())
        ),
    );
    panel.field(
        "Memory",
        format!(
            "{} GiB{}",
            advanced.map(|a| a.memory_gib).unwrap_or(8),
            origin(advanced.is_some())
        ),
    );
    panel.field(
        "Network",
        format!(
            "{}{}",
            match advanced.map(|a| a.network_mode.clone()) {
                Some(NetworkMode::Bridge { interface }) => format!("bridge ({interface})"),
                Some(NetworkMode::Isolated) => "isolated".to_string(),
                Some(NetworkMode::Nat) | None => {
                    if detected.passt_available {
                        "NAT (passt)".to_string()
                    } else {
                        "NAT".to_string()
                    }
                }
            },
            if is_basic { " (default)" } else { "" }
        ),
    );
    let mut out = panel.render_to_string();
    out.push_str(&applied_selection_text(
        basic, advanced, detected, clipboard,
    ));
    out
}

/// The half of the wizard's answers that are *installed* rather than
/// configured: worth spelling out, because they are what the wizard does
/// after creation on its own.
fn applied_selection_text(
    basic: &BasicResult,
    advanced: Option<&AdvancedConfig>,
    detected: &HardwareDefaults,
    clipboard: bool,
) -> String {
    let mut applied: Vec<(String, String)> = Vec::new();
    if clipboard {
        applied.push((
            "clipboard agent".to_string(),
            "spice-vdagent — copy/paste between host and guest".to_string(),
        ));
    }
    if let BasicResult::Android(_) = basic {
        let arm = resolve_arm_translator(advanced, detected);
        if arm != CliArmTranslator::None {
            applied.push((
                "arm translation".to_string(),
                format!("{arm} — runs ARM apps on this x86_64 host"),
            ));
        }
    }

    let mut out = String::new();
    out.push('\n');
    if applied.is_empty() {
        out.push_str("  nothing is installed inside the guest after creation.\n");
    } else {
        out.push_str("  installed in the guest right after creation:\n");
        let width = applied
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0);
        for (label, detail) in &applied {
            out.push_str(&format!("    {label:<width$}  {detail}\n"));
        }
    }
    out.push_str("  the disk is thin-provisioned: it starts small and grows with use.\n");
    out
}

pub(crate) fn format_nat_backend(passt_available: bool) -> NatBackend {
    if passt_available {
        NatBackend::Passt
    } else {
        NatBackend::Slirp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nat_backend_tracks_passt_availability() {
        assert_eq!(format_nat_backend(true), NatBackend::Passt);
        assert_eq!(format_nat_backend(false), NatBackend::Slirp);
    }

    #[test]
    fn summary_of_a_linux_vm_keeps_the_wizard_answers_readable() {
        let basic = BasicResult::Linux(super::super::basic::LinuxBasicResult {
            name: "my-linux".into(),
            iso_path: "/isos/cachyos.iso".into(),
            disk_size_gib: 128,
            instances_root: "/home/user/.andler/instances".into(),
            enable_uefi: true,
        });
        let rendered = summary_text(&basic, None, &super::super::build::sample_detected());

        assert!(rendered.contains("my-linux"), "{rendered}");
        assert!(rendered.contains("Linux VM"), "{rendered}");
        assert!(rendered.contains("/isos/cachyos.iso"), "{rendered}");
        assert!(rendered.contains("128 GiB"), "{rendered}");
        assert!(
            rendered.contains("spice-vdagent"),
            "clipboard sharing is on by default, so the guest agent is announced: {rendered}"
        );
    }

    #[test]
    fn summary_without_guest_selections_says_so() {
        let basic = BasicResult::Linux(super::super::basic::LinuxBasicResult {
            name: "bare".into(),
            iso_path: String::new(),
            disk_size_gib: 64,
            instances_root: "/tmp/instances".into(),
            enable_uefi: true,
        });
        let advanced = AdvancedConfig {
            clipboard_enabled: false,
            ..Default::default()
        };

        let rendered = summary_text(
            &basic,
            Some(&advanced),
            &super::super::build::sample_detected(),
        );

        assert!(
            rendered.contains("nothing is installed inside the guest after creation."),
            "{rendered}"
        );
    }

    #[test]
    fn summary_of_an_android_vm_lists_what_will_be_installed_inside_it() {
        let basic = BasicResult::Android(super::super::basic::AndroidBasicResult {
            name: "my-android".into(),
            base_image: "/cache/android13-vanilla/base.qcow2".into(),
            base_image_auto_resolved: true,
            android_version: crate::CliAndroidVersion::Android13,
            gapps: true,
            disk_size_gib: 256,
            instances_root: "/home/user/.andler/instances".into(),
            linked: false,
        });
        let advanced = AdvancedConfig {
            clipboard_enabled: true,
            gapps: true,
            ..Default::default()
        };

        let rendered = summary_text(
            &basic,
            Some(&advanced),
            &super::super::build::sample_detected(),
        );

        assert!(rendered.contains("Android VM"), "{rendered}");
        assert!(rendered.contains("my-android"), "{rendered}");
        assert!(rendered.contains("GApps"), "{rendered}");
        assert!(
            rendered.contains("spice-vdagent"),
            "clipboard sharing must announce the guest-side agent: {rendered}"
        );
        // The translator is named the one way it is named everywhere else:
        // lowercase, like the CLI flag and the log field.
        assert!(
            rendered.contains("libndk") && !rendered.contains("Libndk"),
            "the ARM translator must be spelled lowercase: {rendered}"
        );
        assert!(
            rendered.contains("runs ARM apps on this x86_64 host"),
            "{rendered}"
        );
    }
}
