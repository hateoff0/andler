use std::path::{Path, PathBuf};

use andler_core::config::{
    ConfigDraft, DiskDraft, DiskSource, DraftKind, FirmwareDraft, HostFirmware,
};
use andler_core::{AndroidBootMode, AndroidProfile, CdromBus, InstanceConfig, InstanceKind};
use andler_firmware::HardwareDefaults;
use andler_rpc::proto::{CreateAndroidInstanceRequest, CreateInstanceRequest};

use crate::helpers::emit_json;
use crate::instance_file::{InstanceFile, TemplateFile};
use crate::preview::Resolved;
use crate::wizard::{PartialArgs, WizardKind};
use crate::{CliAndroidVersion, CliArmTranslator, CliCdromBus, CliKind};

pub(crate) fn default_instances_root() -> String {
    andler_core::paths::instances_root()
        .to_string_lossy()
        .into_owned()
}

/// The host OVMF pair the resolver falls back to, as `andler-firmware`
/// discovered it. An absent pair resolves to no firmware, which pins
/// `enable_uefi` off instead of emitting an empty pflash path.
pub(crate) fn host_firmware(detected: &HardwareDefaults) -> HostFirmware {
    match &detected.ovmf {
        Ok(found) => HostFirmware {
            ovmf_code_path: Some(found.code.clone()),
            ovmf_vars_template: Some(found.vars_template.clone()),
        },
        Err(_) => HostFirmware::default(),
    }
}

/// A creation resolved to one `InstanceConfig` by the one resolver. Every
/// front-end (CLI flags + template, instance file, wizard answers) builds a
/// `ConfigDraft` and resolves it here; every consumer (create, `--dry-run`,
/// `--verify`) reads the result instead of re-deriving defaults or validation.
#[derive(Debug, Clone)]
pub(crate) struct Creation {
    pub cfg: InstanceConfig,
    pub base_image_path: String,
    pub instances_root: String,
}

impl Creation {
    pub(crate) fn resolve(
        draft: &ConfigDraft,
        instances_root: String,
        host: &HostFirmware,
    ) -> Result<Self, String> {
        let base_image_path = match &draft.disk.source {
            DiskSource::BaseImage { path, .. } => path.to_string_lossy().into_owned(),
            DiskSource::Fresh => String::new(),
        };
        Ok(Self {
            cfg: draft.resolve(host)?,
            base_image_path,
            instances_root,
        })
    }

    /// The one validation gate: the same `InstanceConfig::validate` the daemon
    /// runs on create, so the CLI can never accept what the daemon rejects.
    pub(crate) fn validation(&self) -> Result<(), String> {
        self.cfg.validate()
    }

    pub(crate) fn into_request(self) -> ProtoRequest {
        match &self.cfg.kind {
            InstanceKind::LinuxVm {
                iso_path,
                cdrom_bus,
            } => ProtoRequest::Linux(Box::new(linux_request(
                &self.cfg,
                iso_path,
                *cdrom_bus,
                &self.instances_root,
            ))),
            InstanceKind::AndroidVm { android_profile } => {
                ProtoRequest::Android(Box::new(android_request(
                    &self.cfg,
                    android_profile,
                    &self.base_image_path,
                    &self.instances_root,
                )))
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum ProtoRequest {
    Linux(Box<CreateInstanceRequest>),

    Android(Box<CreateAndroidInstanceRequest>),
}

fn linux_request(
    cfg: &InstanceConfig,
    iso_path: &Path,
    cdrom_bus: CdromBus,
    instances_root: &str,
) -> CreateInstanceRequest {
    let mut req = CreateInstanceRequest {
        name: cfg.name.clone(),
        iso_path: iso_path.to_string_lossy().into_owned(),
        cpu: Some(cfg.cpu.clone().into()),
        memory: Some(cfg.memory.clone().into()),
        disk: Some(cfg.disk.clone().into()),
        display: Some(cfg.display.into()),
        gpu: Some(cfg.gpu.clone().into()),
        network: Some(cfg.network.clone().into()),
        firmware: Some(cfg.firmware.clone().into()),
        audio: Some(cfg.audio.into()),
        input: Some(cfg.input.into()),
        autostart: cfg.autostart,
        instances_root: instances_root.to_string(),
        ..Default::default()
    };
    req.set_cdrom_bus(cdrom_bus.into());
    req
}

fn android_request(
    cfg: &InstanceConfig,
    profile: &AndroidProfile,
    base_image_path: &str,
    instances_root: &str,
) -> CreateAndroidInstanceRequest {
    CreateAndroidInstanceRequest {
        name: cfg.name.clone(),
        profile: Some(profile.clone().into()),
        base_image_path: base_image_path.to_string(),
        instances_root: instances_root.to_string(),
        overlay_size_bytes: cfg.disk.size_bytes,
        ovmf_vars_template: cfg.firmware.ovmf_vars_path.to_string_lossy().into_owned(),
        linked_overlay: cfg.disk.base_image.is_some(),
    }
}

fn created_json(id: &str) -> serde_json::Value {
    serde_json::json!({ "instance_id": id })
}

#[allow(clippy::too_many_arguments)] // mirrors all create CLI flags; splitting adds indirection for no benefit
pub async fn handle(
    client: &mut crate::TracedClient,
    file: Option<PathBuf>,
    kind: Option<CliKind>,
    name: Option<String>,
    ovmf_vars_template: Option<String>,
    iso_path: Option<String>,
    disk_path: Option<String>,
    disk_size_gib: Option<u64>,
    compact_on_shutdown: bool,
    cdrom_bus: CliCdromBus,
    no_uefi: bool,
    quick: bool,
    template: Option<String>,
    dry_run: bool,
    verify: bool,
    android_version: Option<CliAndroidVersion>,
    base_image_path: Option<String>,
    gapps: bool,
    microg: bool,
    arm_translator: Option<CliArmTranslator>,
    instances_root: String,
    overlay_size_gib: u64,
    linked_overlay: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_file = file.is_some();
    let has_kind = kind.is_some();

    if has_file && has_kind {
        return Err("--file and --kind are mutually exclusive".into());
    }

    if has_file && quick {
        return Err(
            "--quick and --file are mutually exclusive — --file already provides all configuration"
                .into(),
        );
    }

    if quick && !has_kind {
        return Err("--quick requires --kind to specify VM type".into());
    }

    if dry_run && quick {
        return Err(
            "--dry-run and --quick are mutually exclusive — --quick creates immediately, --dry-run never creates"
                .into(),
        );
    }

    if verify && quick {
        return Err(
            "--verify and --quick are mutually exclusive — --quick creates immediately, --verify never creates"
                .into(),
        );
    }

    if dry_run && verify {
        return Err("--dry-run and --verify are mutually exclusive — pass one or the other".into());
    }

    // Every flag lives in one layer; the wizard path and the CLI-flag path
    // read the same layer, so no flag is silently dropped by either.
    let flags = PartialArgs {
        kind: kind.map(|k| match k {
            CliKind::Linux => WizardKind::Linux,
            CliKind::Android => WizardKind::Android,
        }),
        name,
        iso_path,
        base_image_path,
        instances_root: Some(instances_root),
        disk_path,
        disk_size_gib,
        overlay_size_gib: Some(overlay_size_gib),
        android_version,
        arm_translator,
        cdrom_bus: Some(cdrom_bus),
        compact_on_shutdown: Some(compact_on_shutdown),
        enable_uefi: if no_uefi { Some(false) } else { None },
        ovmf_vars_template,
        gapps,
        microg,
        linked: linked_overlay,
        template,
        quick,
    };

    if !has_file {
        let linux_required = flags.kind == Some(WizardKind::Linux)
            && (flags.name.is_none() || flags.iso_path.is_none() || flags.disk_path.is_none());
        let android_required = flags.kind == Some(WizardKind::Android) && flags.name.is_none();
        let needs_wizard =
            flags.quick || flags.kind.is_none() || linux_required || android_required;

        if needs_wizard {
            if dry_run {
                return Err(
                    "--dry-run requires --file or all CLI-mode flags for the chosen --kind \
                     (the interactive wizard already shows a full summary before creating, so \
                     --dry-run with a bare `andler create` isn't supported)"
                        .into(),
                );
            }
            if verify {
                return Err(
                    "--verify requires --file or all CLI-mode flags for the chosen --kind \
                     (the interactive wizard already shows a full summary before creating, so \
                     --verify with a bare `andler create` isn't supported)"
                        .into(),
                );
            }

            // One code path for both the interactive wizard and `--quick`:
            // resolve the answers, create the VM, then install the guest-side
            // selections the answers imply (ARM translator, clipboard agent).
            return crate::wizard::handle_wizard(client, flags).await;
        }
    }

    let detected = andler_firmware::detect_all();
    let host = crate::create::host_firmware(&detected);

    let (draft, instances_root) = if let Some(file) = file {
        let file_draft = InstanceFile::load(&file)?.into_draft()?;
        (file_draft.draft, file_draft.instances_root)
    } else {
        let kind = flags
            .kind
            .ok_or("--kind is required for CLI mode without --file")?;
        let template = match flags.template.as_deref() {
            Some(_) if kind == WizardKind::Android => {
                return Err(
                    "--template is only supported with --kind linux in this phase (Android requests \
                     carry no config sections yet)"
                        .into(),
                );
            }
            Some(name) => Some(TemplateFile::load(name)?),
            None => None,
        };
        let draft = match kind {
            WizardKind::Linux => linux_draft(&flags, template.as_ref())?,
            WizardKind::Android => android_draft(&flags)?,
        };
        let instances_root = flags
            .instances_root
            .clone()
            .unwrap_or_else(crate::create::default_instances_root);
        (draft, instances_root)
    };

    let creation = Creation::resolve(&draft, instances_root, &host)?;

    if dry_run {
        let resolved = Resolved::from_creation(&creation, &host);
        return crate::preview::report_dry_run(&resolved, json);
    }
    if verify {
        let resolved = Resolved::from_creation(&creation, &host);
        return exit_on_verify_result(crate::verify::verify(&resolved, json)?);
    }

    if let Err(issue) = creation.validation() {
        return Err(format!("invalid configuration: {issue}").into());
    }

    let created_name = creation.cfg.name.clone();
    let id = match creation.into_request() {
        ProtoRequest::Linux(req) => client.create_instance(*req).await?.into_inner().instance_id,
        ProtoRequest::Android(req) => {
            client
                .create_android_instance(*req)
                .await?
                .into_inner()
                .instance_id
        }
    };
    if json {
        emit_json(&created_json(&id))?;
    } else {
        println!(
            "Created instance {created_name} ({})",
            crate::helpers::short_id(&id)
        );
    }

    Ok(())
}

fn exit_on_verify_result(all_passed: bool) -> Result<(), Box<dyn std::error::Error>> {
    if all_passed {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// The CLI-flag path's draft: every section the flags carry, the template's
/// sections under them, and nothing else — the resolver fills the rest.
pub(crate) fn linux_draft(
    flags: &PartialArgs,
    template: Option<&TemplateFile>,
) -> Result<ConfigDraft, String> {
    let name = flags
        .name
        .clone()
        .ok_or("--name is required for --kind linux")?;
    let iso_path = flags
        .iso_path
        .clone()
        .ok_or("--iso-path is required for --kind linux")?;
    let disk_path = flags
        .disk_path
        .clone()
        .ok_or("--disk-path is required for --kind linux")?;
    let (iso_path, disk_path) = validate_linux_paths(&iso_path, &disk_path)?;

    let cdrom_bus = match flags.cdrom_bus.unwrap_or(CliCdromBus::Auto) {
        CliCdromBus::Auto => {
            CdromBus::recommended_for_iso_filename(std::path::Path::new(&iso_path))
        }
        CliCdromBus::Virtio => CdromBus::VirtioScsi,
        CliCdromBus::Ide => CdromBus::Ide,
    };

    Ok(ConfigDraft {
        name,
        kind: DraftKind::Linux {
            iso_path: PathBuf::from(iso_path),
            cdrom_bus,
        },
        disk: DiskDraft {
            path: PathBuf::from(disk_path),
            source: DiskSource::Fresh,
            size_gib: flags.disk_size_gib,
            compact_on_shutdown: flags.compact_on_shutdown.unwrap_or(false),
            snapshot_timeout_secs: None,
        },
        firmware: FirmwareDraft {
            enable_uefi: flags.enable_uefi,
            ovmf_vars_template: flags.ovmf_vars_template.clone().map(PathBuf::from),
        },
        autostart: false,
        cpu: template.and_then(|t| t.cpu.clone()),
        memory: template.and_then(|t| t.memory.clone()),
        display: template.and_then(|t| t.display),
        gpu: template.and_then(|t| t.gpu.clone()),
        network: template.and_then(|t| t.network.clone()),
        audio: template.and_then(|t| t.audio),
        input: template.and_then(|t| t.input),
    })
}

/// The CLI-flag path's Android draft. Android instances take their config
/// sections from the profile, so the flags that only reach sections
/// (`--template`, and a hand-written section in a TOML file) are refused by
/// the resolver rather than dropped here.
pub(crate) fn android_draft(flags: &PartialArgs) -> Result<ConfigDraft, String> {
    let name = flags
        .name
        .clone()
        .ok_or("--name is required for --kind android")?;
    let android_version = flags
        .android_version
        .ok_or("--android-version is required for --kind android")?;
    let base_image_path = match &flags.base_image_path {
        Some(path) => validate_base_image_path(path)?,
        None => String::new(),
    };
    let instances_root = flags
        .instances_root
        .clone()
        .unwrap_or_else(crate::create::default_instances_root);

    let profile = AndroidProfile {
        android_version: android_version.into(),
        gapps: flags.gapps,
        microg: flags.microg,
        arm_translator: flags
            .arm_translator
            .unwrap_or(CliArmTranslator::None)
            .into(),
        boot_mode: AndroidBootMode::Android,
        base_image_pin: None,
    };

    Ok(ConfigDraft {
        name,
        kind: DraftKind::Android { profile },
        disk: DiskDraft {
            path: PathBuf::from(&instances_root).join("disk.qcow2"),
            source: DiskSource::BaseImage {
                path: PathBuf::from(base_image_path),
                linked: flags.linked,
            },
            size_gib: flags.overlay_size_gib,
            compact_on_shutdown: false,
            snapshot_timeout_secs: None,
        },
        firmware: FirmwareDraft {
            enable_uefi: flags.enable_uefi,
            ovmf_vars_template: flags.ovmf_vars_template.clone().map(PathBuf::from),
        },
        autostart: false,
        cpu: None,
        memory: None,
        display: None,
        gpu: None,
        network: None,
        audio: None,
        input: None,
    })
}

fn validate_linux_paths(iso_path: &str, disk_path: &str) -> Result<(String, String), String> {
    let canonical_iso = if iso_path.is_empty() {
        String::new()
    } else {
        let canonical = std::fs::canonicalize(iso_path)
            .map_err(|e| format!("ISO file not found: {iso_path} ({e})"))?;
        canonical.to_string_lossy().into_owned()
    };

    let disk = std::path::Path::new(disk_path);
    let canonical_disk = match disk.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
                format!("disk directory does not exist: {} ({e})", parent.display())
            })?;
            let file_name = disk
                .file_name()
                .ok_or_else(|| format!("invalid disk path: {disk_path}"))?;
            canonical_parent
                .join(file_name)
                .to_string_lossy()
                .into_owned()
        }
        _ => disk_path.to_string(),
    };

    Ok((canonical_iso, canonical_disk))
}

fn validate_base_image_path(base_image_path: &str) -> Result<String, String> {
    let canonical_base_image = std::fs::canonicalize(base_image_path)
        .map_err(|e| format!("base image not found: {base_image_path} ({e})"))?
        .to_string_lossy()
        .into_owned();

    Ok(canonical_base_image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wizard::advanced::AdvancedConfig;
    use crate::wizard::LinuxBasicResult;
    use andler_core::config::HostFirmware;
    use andler_core::{
        CpuConfig, CpuPriority, DisplayConfig, GpuConfig, InstanceConfig, InstanceId, MemoryConfig,
        NetworkConfig, Resolution,
    };

    #[test]
    fn validate_linux_paths_empty_iso_is_allowed() {
        assert!(validate_linux_paths("", "/tmp").is_ok());
    }

    #[test]
    fn validate_linux_paths_missing_iso_is_rejected() {
        let err = validate_linux_paths("/nonexistent/path/to.iso", "/tmp").unwrap_err();
        assert!(err.contains("ISO file not found"));
    }

    #[test]
    fn validate_linux_paths_missing_disk_directory_is_rejected() {
        let err = validate_linux_paths("", "/nonexistent/andler-test-dir/disk.qcow2").unwrap_err();
        assert!(err.contains("disk directory does not exist"));
    }

    #[test]
    fn validate_linux_paths_bare_relative_disk_filename_is_allowed() {
        assert!(validate_linux_paths("", "disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_existing_disk_directory_is_allowed() {
        assert!(validate_linux_paths("", "/tmp/disk.qcow2").is_ok());
    }

    #[test]
    fn validate_linux_paths_canonicalizes_disk_directory() {
        let (_, disk) = validate_linux_paths("", "/tmp/../tmp/my-disk.qcow2").unwrap();
        assert!(!disk.contains(".."));
        assert!(disk.ends_with("my-disk.qcow2"));
    }

    #[test]
    fn validate_linux_paths_canonicalizes_iso_path() {
        let (iso, _) = validate_linux_paths("/tmp/../tmp", "disk.qcow2").unwrap();
        assert!(!iso.contains(".."));
    }

    #[test]
    fn validate_base_image_path_missing_is_rejected() {
        let err = validate_base_image_path("/nonexistent/base.qcow2").unwrap_err();
        assert!(err.contains("base image not found"));
    }

    #[test]
    fn validate_base_image_path_existing_is_allowed() {
        assert!(validate_base_image_path("/tmp").is_ok());
    }

    #[test]
    fn created_json_serializes_instance_id() {
        let json = created_json("0123456789abcdef0123456789abcdef");
        assert_eq!(json["instance_id"], "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn validate_base_image_path_canonicalizes() {
        let base_image = validate_base_image_path("/tmp/../tmp").unwrap();
        assert!(!base_image.contains(".."));
    }

    // --- one resolver, three ways to ask ------------------------------------

    /// The identity is the one field creation is allowed to differ on (the
    /// daemon assigns it), so two resolved configs are comparable once it is
    /// pinned.
    fn identity_stripped(cfg: InstanceConfig) -> InstanceConfig {
        InstanceConfig {
            id: "0"
                .repeat(andler_core::INSTANCE_ID_HEX_LEN)
                .parse::<InstanceId>()
                .expect("all-zero id parses"),
            ..cfg
        }
    }

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "andler-create-convergence-{}-{name}",
            std::process::id()
        ));
        let instances = dir.join("instances");
        std::fs::create_dir_all(&instances).expect("fixture dir");
        std::fs::write(dir.join("ubuntu-24.04.iso"), b"iso").expect("fixture iso");
        std::fs::write(dir.join("VARS.fd"), b"vars template").expect("fixture vars");
        std::fs::write(dir.join("base.qcow2"), b"base image").expect("fixture base image");
        dir
    }

    /// The host firmware every path sees, so the comparison does not depend on
    /// what the machine andler runs on has installed.
    fn fixture_host() -> HostFirmware {
        HostFirmware {
            ovmf_code_path: Some(PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd")),
            ovmf_vars_template: Some(PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        }
    }

    /// The sections all three Linux paths below describe, as a template (the
    /// flag path's only way to set sections), a TOML file and a wizard answer
    /// set. Deliberately real values, and deliberately not all defaults: a
    /// divergence in any single section changes the comparison.
    fn linux_template() -> TemplateFile {
        TemplateFile {
            cpu: Some(CpuConfig {
                cores: 8,
                sockets: 1,
                threads: 1,
                affinity: None,
                priority: CpuPriority::Normal,
            }),
            memory: Some(MemoryConfig {
                size_bytes: 16 * andler_core::MemoryConfig::GIB,
                ballooning: false,
                zram: false,
                ksm: true,
                mem_lock: false,
                hugepages: false,
            }),
            display: Some(DisplayConfig {
                resolution: Resolution::new(2560, 1440),
                dpi: 96,
                fps_limit: DisplayConfig::FPS_UNLIMITED,
                display_engine: andler_core::DisplayEngine::Gtk,
                fullscreen: true,
            }),
            gpu: Some(GpuConfig {
                render_backend: andler_core::RenderBackend::VirGl,
                hostmem_bytes: 8192 * andler_core::GpuConfig::MIB,
                blob: true,
                gl: true,
            }),
            network: Some(NetworkConfig {
                mode: andler_core::NetworkMode::Nat,
                device_model: "virtio-net-pci".to_string(),
                nat_backend: andler_core::NatBackend::Passt,
                port_forwards: Vec::new(),
            }),
            audio: Some(andler_core::AudioConfig {
                backend: andler_core::AudioBackend::None,
                device: andler_core::AudioDevice::VirtioSound,
            }),
            input: Some(andler_core::InputConfig {
                pointer_mode: andler_core::PointerMode::Mouse,
                hide_host_cursor: true,
                clipboard_enabled: false,
            }),
        }
    }

    fn linux_toml(dir: &std::path::Path) -> String {
        format!(
            r#"
name = "converged"
iso_path = "{iso}"
disk_path = "{disk}"
disk_size_gib = 100
cdrom_bus = "virtio"

[cpu]
cores = 8
sockets = 1
threads = 1
priority = "normal"

[memory]
size_bytes = 17179869184
ballooning = false
zram = false
ksm = true
mem_lock = false
hugepages = false

[display]
resolution = {{ width = 2560, height = 1440 }}
dpi = 96
fps_limit = 0
engine = "gtk"
fullscreen = true

[gpu]
render_backend = "virgl"
hostmem_bytes = 8589934592
blob = true
gl = true

[network]
mode = "nat"
device_model = "virtio-net-pci"
nat_backend = "passt"

[audio]
backend = "none"
device = "virtiosound"

[input]
pointer_mode = "mouse"
hide_host_cursor = true
clipboard_enabled = false
"#,
            iso = dir.join("ubuntu-24.04.iso").display(),
            disk = dir.join("instances").join("disk.qcow2").display(),
        )
    }

    fn wizard_answers() -> AdvancedConfig {
        AdvancedConfig {
            cdrom_bus: Some(andler_core::CdromBus::VirtioScsi),
            compact_on_shutdown: false,
            gpu_render: andler_core::RenderBackend::VirGl,
            gpu_memory_mib: 8192,
            display_resolution: Resolution::new(2560, 1440),
            fullscreen: true,
            audio_backend: andler_core::AudioBackend::None,
            clipboard_enabled: false,
            input_pointer: andler_core::PointerMode::Mouse,
            cpu_cores: 8,
            memory_gib: 16,
            arm_translator: None,
            gapps: false,
            network_mode: andler_core::NetworkMode::Nat,
            bridge_interface: None,
            linked_overlay: false,
        }
    }

    /// The same Linux VM asked for three ways — CLI flags plus a template, an
    /// instance file, and a wizard answer set — resolves to the identical
    /// config, defaults and validation included.
    #[test]
    fn flags_a_toml_file_and_wizard_answers_resolve_to_the_same_config() {
        let dir = workspace("linux");
        let instances_root = dir.join("instances").to_string_lossy().into_owned();
        let host = fixture_host();

        let flags = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("converged".to_string()),
            iso_path: Some(dir.join("ubuntu-24.04.iso").to_string_lossy().into_owned()),
            disk_path: Some(
                dir.join("instances")
                    .join("disk.qcow2")
                    .to_string_lossy()
                    .into_owned(),
            ),
            instances_root: Some(instances_root.clone()),
            disk_size_gib: Some(100),
            cdrom_bus: Some(CliCdromBus::Virtio),
            ..Default::default()
        };
        let from_flags = Creation::resolve(
            &linux_draft(&flags, Some(&linux_template())).expect("flag draft"),
            instances_root.clone(),
            &host,
        )
        .expect("flag path resolves");

        let file = crate::instance_file::InstanceFile::into_draft(
            toml::from_str::<crate::instance_file::InstanceFile>(&linux_toml(&dir))
                .expect("fixture TOML parses"),
        )
        .expect("file draft");
        let from_file =
            Creation::resolve(&file.draft, file.instances_root, &host).expect("file path resolves");

        let detected = crate::wizard::build::sample_detected();
        let wizard_flags = PartialArgs {
            instances_root: Some(instances_root.clone()),
            ..Default::default()
        };
        let basic = LinuxBasicResult {
            name: "converged".to_string(),
            iso_path: dir.join("ubuntu-24.04.iso").to_string_lossy().into_owned(),
            disk_size_gib: 100,
            instances_root: instances_root.clone(),
            enable_uefi: Some(true),
        };
        let wizard_draft = crate::wizard::build::linux_draft(
            &basic,
            Some(&wizard_answers()),
            &wizard_flags,
            &detected,
        )
        .expect("wizard draft");
        let mut wizard_host = crate::create::host_firmware(&detected);
        wizard_host.ovmf_vars_template = host.ovmf_vars_template.clone();
        wizard_host.ovmf_code_path = host.ovmf_code_path.clone();
        let from_wizard = Creation::resolve(&wizard_draft, instances_root, &wizard_host)
            .expect("wizard path resolves");

        assert!(
            from_file.cfg.firmware.ovmf_vars_path.as_os_str().is_empty(),
            "no path given an explicit template: the daemon provisions its own"
        );
        let expected = identity_stripped(from_flags.cfg);
        assert_eq!(identity_stripped(from_file.cfg), expected);
        assert_eq!(identity_stripped(from_wizard.cfg), expected);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The same input that is *not* creatable is refused with the same verdict
    /// by all three paths — the divergence today is that `--dry-run` reports a
    /// preview for a config creation rejects.
    #[test]
    fn an_uncreatable_input_is_refused_identically_by_every_path() {
        let dir = workspace("linux-invalid");
        let instances_root = dir.join("instances").to_string_lossy().into_owned();
        let host = fixture_host();

        let mut broken = linux_template();
        broken.cpu = Some(CpuConfig {
            cores: 0,
            ..CpuConfig::reference_default()
        });
        let flags = PartialArgs {
            kind: Some(WizardKind::Linux),
            name: Some("converged".to_string()),
            iso_path: Some(dir.join("ubuntu-24.04.iso").to_string_lossy().into_owned()),
            disk_path: Some(
                dir.join("instances")
                    .join("disk.qcow2")
                    .to_string_lossy()
                    .into_owned(),
            ),
            instances_root: Some(instances_root.clone()),
            disk_size_gib: Some(100),
            cdrom_bus: Some(CliCdromBus::Virtio),
            ..Default::default()
        };
        let from_flags = Creation::resolve(
            &linux_draft(&flags, Some(&broken)).expect("flag draft"),
            instances_root.clone(),
            &host,
        )
        .expect("resolution succeeds; validation is what fails");

        let toml_text = linux_toml(&dir).replace("cores = 8", "cores = 0");
        let file = crate::instance_file::InstanceFile::into_draft(
            toml::from_str::<crate::instance_file::InstanceFile>(&toml_text)
                .expect("fixture TOML parses"),
        )
        .expect("file draft");
        let from_file =
            Creation::resolve(&file.draft, file.instances_root, &host).expect("file resolves");

        let mut answers = wizard_answers();
        answers.cpu_cores = 0;
        let detected = crate::wizard::build::sample_detected();
        let wizard_flags = PartialArgs {
            instances_root: Some(instances_root.clone()),
            ..Default::default()
        };
        let basic = LinuxBasicResult {
            name: "converged".to_string(),
            iso_path: dir.join("ubuntu-24.04.iso").to_string_lossy().into_owned(),
            disk_size_gib: 100,
            instances_root: instances_root.clone(),
            enable_uefi: Some(true),
        };
        let wizard_draft =
            crate::wizard::build::linux_draft(&basic, Some(&answers), &wizard_flags, &detected)
                .expect("wizard draft");
        let mut wizard_host = crate::create::host_firmware(&detected);
        wizard_host.ovmf_vars_template = host.ovmf_vars_template.clone();
        wizard_host.ovmf_code_path = host.ovmf_code_path.clone();
        let from_wizard =
            Creation::resolve(&wizard_draft, instances_root, &wizard_host).expect("resolves");

        let verdict = from_flags
            .validation()
            .expect_err("zero cores is not creatable");
        assert_eq!(verdict, "cpu.cores must be at least 1");
        assert_eq!(from_file.validation(), Err(verdict.clone()));
        assert_eq!(from_wizard.validation(), Err(verdict));

        let resolved = Resolved::from_creation(&from_flags, &host);
        assert_eq!(
            resolved.validation,
            Err("cpu.cores must be at least 1".to_string())
        );
        assert!(
            crate::preview::report_dry_run(&resolved, false).is_err(),
            "--dry-run must not report a preview for a config creation refuses"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
