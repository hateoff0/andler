//! Full key-path table for `andler config set` / `andler config status`.
//!
//! Every leaf of `InstanceConfig` a user could reasonably set gets a row: a
//! getter producing the canonical string form, a setter parsing user input,
//! and a `live` marker for keys that also apply to a running guest. Identity
//! fields (id, kind, backend, disk path, ...) are listed as immutable rows so
//! `config set` rejects them with a reason instead of "unknown key".
//!
//! Row values use the CLI's input forms: `1920x1080`, `8G`, `true`,
//! lowercase enum names. `config status` diffs two configs with the same
//! canonical strings, so what you type and what you see agree.

use crate::config::{
    AudioBackend, AudioDevice, CpuPriority, DisplayEngine, InstanceConfig, InstanceKind,
    NatBackend, NetworkMode, PointerMode, PortForwardProtocol, RenderBackend,
};
use crate::sizes::{format_size, parse_size};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigKeyError {
    Unknown(String),
    Immutable {
        key: String,
        reason: &'static str,
    },
    InvalidValue {
        key: String,
        value: String,
        reason: String,
    },
}

impl std::fmt::Display for ConfigKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigKeyError::Unknown(key) => {
                write!(
                    f,
                    "unknown config key `{key}`; run `andler config status` for the full list"
                )
            }
            ConfigKeyError::Immutable { key, reason } => {
                write!(f, "config key `{key}` cannot be set: {reason}")
            }
            ConfigKeyError::InvalidValue { key, value, reason } => write!(
                f,
                "invalid value `{value}` for config key `{key}`: {reason}"
            ),
        }
    }
}

impl std::error::Error for ConfigKeyError {}

type Setter = fn(&mut InstanceConfig, &str) -> Result<(), String>;

pub struct ConfigKey {
    pub key: &'static str,
    get: fn(&InstanceConfig) -> String,
    set: Option<Setter>,
    immutable_reason: Option<&'static str>,
    /// Key applies to a running guest immediately (RPC path), not only at
    /// next start.
    pub live: bool,
}

fn parse_u32(key: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("`{key}` expects a non-negative integer, got `{value}`"))
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("expected true or false, got `{value}`")),
    }
}

fn parse_enum<T>(value: &str, table: &[(&str, T)]) -> Result<T, String>
where
    T: Copy,
{
    let needle = value.trim().to_ascii_lowercase().replace(['-', '_'], "");
    table
        .iter()
        .find(|(name, _)| *name == needle)
        .map(|(_, variant)| *variant)
        .ok_or_else(|| {
            let names = table
                .iter()
                .map(|(name, _)| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("expected one of {names}, got `{value}`")
        })
}

fn set_cpu_cores(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.cpu.cores = parse_u32("cpu.cores", value)?;
    Ok(())
}

fn set_cpu_sockets(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.cpu.sockets = parse_u32("cpu.sockets", value)?;
    Ok(())
}

fn set_cpu_threads(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.cpu.threads = parse_u32("cpu.threads", value)?;
    Ok(())
}

fn cpu_priority_get(cfg: &InstanceConfig) -> String {
    enum_lower(&cfg.cpu.priority)
}

fn cpu_priority_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.cpu.priority = parse_enum(
        value,
        &[
            ("low", CpuPriority::Low),
            ("normal", CpuPriority::Normal),
            ("high", CpuPriority::High),
        ],
    )?;
    Ok(())
}

fn memory_size_get(cfg: &InstanceConfig) -> String {
    format_size(cfg.memory.size_bytes)
}

fn memory_size_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    let bytes = parse_size(value)?;
    if bytes == 0 {
        return Err("memory size must be greater than 0".to_string());
    }
    cfg.memory.size_bytes = bytes;
    Ok(())
}

fn bool_str(value: bool) -> String {
    value.to_string()
}

macro_rules! bool_keys {
    ($($section:ident.$field:ident => $get:ident / $set:ident),* $(,)?) => {
        $(
            fn $get(cfg: &InstanceConfig) -> String {
                bool_str(cfg.$section.$field)
            }
            fn $set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
                cfg.$section.$field = parse_bool(value)?;
                Ok(())
            }
        )*
    };
}

bool_keys!(
    memory.ballooning => memory_ballooning_get / memory_ballooning_set,
    memory.zram => memory_zram_get / memory_zram_set,
    memory.ksm => memory_ksm_get / memory_ksm_set,
    disk.thin_provisioning => disk_thin_provisioning_get / disk_thin_provisioning_set,
    disk.trim_on_shutdown => disk_trim_on_shutdown_get / disk_trim_on_shutdown_set,
    disk.compact_on_shutdown => disk_compact_on_shutdown_get / disk_compact_on_shutdown_set,
    display.fullscreen => display_fullscreen_get / display_fullscreen_set,
    gpu.blob => gpu_blob_get / gpu_blob_set,
    gpu.gl => gpu_gl_get / gpu_gl_set,
    input.hide_host_cursor => input_hide_host_cursor_get / input_hide_host_cursor_set,
    input.clipboard_enabled => input_clipboard_enabled_get / input_clipboard_enabled_set,
    firmware.enable_uefi => firmware_enable_uefi_get / firmware_enable_uefi_set,
);

fn disk_snapshot_timeout_get(cfg: &InstanceConfig) -> String {
    match cfg.disk.snapshot_timeout_secs {
        Some(secs) => secs.to_string(),
        None => "none".to_string(),
    }
}

fn disk_snapshot_timeout_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.disk.snapshot_timeout_secs = match value.trim().to_ascii_lowercase().as_str() {
        "none" | "off" => None,
        _ => Some(parse_u32("disk.snapshot_timeout_secs", value)? as u64),
    };
    Ok(())
}

fn resolution_get(cfg: &InstanceConfig) -> String {
    format!(
        "{}x{}",
        cfg.display.resolution.width, cfg.display.resolution.height
    )
}

fn resolution_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    let (width, height) = value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .filter(|(w, h)| *w > 0 && *h > 0)
        .ok_or_else(|| "expected WxH, e.g. 1920x1080".to_string())?;
    cfg.display.resolution = crate::config::Resolution::new(width, height);
    Ok(())
}

fn render_backend_get(cfg: &InstanceConfig) -> String {
    match &cfg.gpu.render_backend {
        RenderBackend::Passthrough { gpu_pci_id } => format!("passthrough:{gpu_pci_id}"),
        other => enum_lower(other),
    }
}

fn render_backend_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.gpu.render_backend = match value.trim().to_ascii_lowercase().as_str() {
        "venus" => RenderBackend::Venus,
        "virtiogpu" | "virtio-gpu" => RenderBackend::VirtioGpu,
        "virgl" => RenderBackend::VirGl,
        "cpu" | "software" => RenderBackend::Cpu,
        _ => {
            if let Some(pci) = value
                .trim()
                .strip_prefix("passthrough:")
                .filter(|pci| !pci.is_empty())
            {
                RenderBackend::Passthrough {
                    gpu_pci_id: pci.to_string(),
                }
            } else {
                return Err(
                    "expected one of `venus`, `virtiogpu`, `virgl`, `cpu`, `passthrough:<pci>`"
                        .to_string(),
                );
            }
        }
    };
    Ok(())
}

fn network_mode_get(cfg: &InstanceConfig) -> String {
    match &cfg.network.mode {
        NetworkMode::Bridge { interface } => format!("bridge:{interface}"),
        other => enum_lower(other),
    }
}

fn network_mode_set(cfg: &mut InstanceConfig, value: &str) -> Result<(), String> {
    cfg.network.mode = match value.trim().to_ascii_lowercase().as_str() {
        "nat" => NetworkMode::Nat,
        "isolated" => NetworkMode::Isolated,
        _ => {
            if let Some(interface) = value
                .trim()
                .strip_prefix("bridge:")
                .filter(|iface| !iface.is_empty())
            {
                NetworkMode::Bridge {
                    interface: interface.to_string(),
                }
            } else {
                return Err("expected one of `nat`, `bridge:<interface>`, `isolated`".to_string());
            }
        }
    };
    Ok(())
}

fn android_arm_translator_get(cfg: &InstanceConfig) -> String {
    match &cfg.kind {
        InstanceKind::AndroidVm { android_profile } => enum_lower(&android_profile.arm_translator),
        InstanceKind::LinuxVm { .. } => String::new(),
    }
}

fn android_boot_mode_get(cfg: &InstanceConfig) -> String {
    match &cfg.kind {
        InstanceKind::AndroidVm { android_profile } => android_profile.boot_mode.to_string(),
        InstanceKind::LinuxVm { .. } => String::new(),
    }
}

fn android_version_get(cfg: &InstanceConfig) -> String {
    match &cfg.kind {
        InstanceKind::AndroidVm { android_profile } => {
            format!("{:?}", android_profile.android_version).to_ascii_lowercase()
        }
        InstanceKind::LinuxVm { .. } => String::new(),
    }
}

fn extra_disks_get(cfg: &InstanceConfig) -> String {
    format!("{} disk(s)", cfg.extra_disks.len())
}

fn extra_networks_get(cfg: &InstanceConfig) -> String {
    format!("{} nic(s)", cfg.extra_networks.len())
}

fn enum_lower<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}").to_ascii_lowercase()
}

pub fn config_keys() -> &'static [ConfigKey] {
    static KEYS: &[ConfigKey] = &[
        ConfigKey {
            key: "id",
            get: |cfg| cfg.id.to_string(),
            set: None,
            immutable_reason: Some(
                "the instance id identifies the directory; it cannot be changed",
            ),
            live: false,
        },
        ConfigKey {
            key: "name",
            get: |cfg| cfg.name.clone(),
            set: Some(|cfg, value| {
                let name = value.trim();
                if name.is_empty() {
                    return Err("name must not be empty".to_string());
                }
                cfg.name = name.to_string();
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "kind",
            get: |cfg| {
                match &cfg.kind {
                    InstanceKind::LinuxVm { .. } => "linux",
                    InstanceKind::AndroidVm { .. } => "android",
                }
                .to_string()
            },
            set: None,
            immutable_reason: Some(
                "the VM kind is fixed at creation; create a new instance instead",
            ),
            live: false,
        },
        ConfigKey {
            key: "backend",
            get: |cfg| enum_lower(&cfg.backend),
            set: None,
            immutable_reason: Some("the only supported backend is QEMU"),
            live: false,
        },
        ConfigKey {
            key: "schema_version",
            get: |cfg| cfg.schema_version.to_string(),
            set: None,
            immutable_reason: Some("managed by andlerd schema migrations"),
            live: false,
        },
        ConfigKey {
            key: "cpu.cores",
            get: |cfg| cfg.cpu.cores.to_string(),
            set: Some(set_cpu_cores),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "cpu.sockets",
            get: |cfg| cfg.cpu.sockets.to_string(),
            set: Some(set_cpu_sockets),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "cpu.threads",
            get: |cfg| cfg.cpu.threads.to_string(),
            set: Some(set_cpu_threads),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "cpu.priority",
            get: cpu_priority_get,
            set: Some(cpu_priority_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "cpu.affinity",
            get: |cfg| match &cfg.cpu.affinity {
                Some(affinity) => affinity
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                None => "none".to_string(),
            },
            set: None,
            immutable_reason: Some("edit cpu.affinity in instance.toml directly"),
            live: false,
        },
        ConfigKey {
            key: "memory.size_bytes",
            get: memory_size_get,
            set: Some(memory_size_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "memory.ballooning",
            get: memory_ballooning_get,
            set: Some(memory_ballooning_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "memory.zram",
            get: memory_zram_get,
            set: Some(memory_zram_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "memory.ksm",
            get: memory_ksm_get,
            set: Some(memory_ksm_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "disk.path",
            get: |cfg| cfg.disk.path.display().to_string(),
            set: None,
            immutable_reason: Some("the disk lives at its path; create a new instance to move it"),
            live: false,
        },
        ConfigKey {
            key: "disk.size_bytes",
            get: |cfg| format_size(cfg.disk.size_bytes),
            set: None,
            immutable_reason: Some("use `andler disk resize` to grow the disk"),
            live: false,
        },
        ConfigKey {
            key: "disk.format",
            get: |cfg| enum_lower(&cfg.disk.format),
            set: None,
            immutable_reason: Some("the disk format is fixed at creation"),
            live: false,
        },
        ConfigKey {
            key: "disk.base_image",
            get: |cfg| match &cfg.disk.base_image {
                Some(path) => path.display().to_string(),
                None => "none".to_string(),
            },
            set: None,
            immutable_reason: Some("the base image is pinned at creation"),
            live: false,
        },
        ConfigKey {
            key: "disk.thin_provisioning",
            get: disk_thin_provisioning_get,
            set: Some(disk_thin_provisioning_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "disk.trim_on_shutdown",
            get: disk_trim_on_shutdown_get,
            set: Some(disk_trim_on_shutdown_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "disk.compact_on_shutdown",
            get: disk_compact_on_shutdown_get,
            set: Some(disk_compact_on_shutdown_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "disk.snapshot_timeout_secs",
            get: disk_snapshot_timeout_get,
            set: Some(disk_snapshot_timeout_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "display.resolution",
            get: resolution_get,
            set: Some(resolution_set),
            immutable_reason: None,
            live: true,
        },
        ConfigKey {
            key: "display.dpi",
            get: |cfg| cfg.display.dpi.to_string(),
            set: Some(|cfg, value| {
                cfg.display.dpi = parse_u32("display.dpi", value)?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "display.fps_limit",
            get: |cfg| cfg.display.fps_limit.to_string(),
            set: Some(|cfg, value| {
                cfg.display.fps_limit = parse_u32("display.fps_limit", value)?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "display.display_engine",
            get: |cfg| enum_lower(&cfg.display.display_engine),
            set: Some(|cfg, value| {
                cfg.display.display_engine = parse_enum(
                    value,
                    &[
                        ("sdl", DisplayEngine::Sdl),
                        ("gtk", DisplayEngine::Gtk),
                        ("spice", DisplayEngine::Spice),
                        ("dbus", DisplayEngine::Dbus),
                        ("none", DisplayEngine::None),
                    ],
                )?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "display.fullscreen",
            get: display_fullscreen_get,
            set: Some(display_fullscreen_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "gpu.render_backend",
            get: render_backend_get,
            set: Some(render_backend_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "gpu.hostmem_bytes",
            get: |cfg| format_size(cfg.gpu.hostmem_bytes),
            set: Some(|cfg, value| {
                let bytes = parse_size(value)?;
                if bytes == 0 {
                    return Err("hostmem size must be greater than 0".to_string());
                }
                cfg.gpu.hostmem_bytes = bytes;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "gpu.blob",
            get: gpu_blob_get,
            set: Some(gpu_blob_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "gpu.gl",
            get: gpu_gl_get,
            set: Some(gpu_gl_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "network.mode",
            get: network_mode_get,
            set: Some(network_mode_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "network.device_model",
            get: |cfg| cfg.network.device_model.clone(),
            set: Some(|cfg, value| {
                let model = value.trim();
                if model.is_empty() {
                    return Err("device model must not be empty".to_string());
                }
                cfg.network.device_model = model.to_string();
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "network.nat_backend",
            get: |cfg| enum_lower(&cfg.network.nat_backend),
            set: Some(|cfg, value| {
                cfg.network.nat_backend = parse_enum(
                    value,
                    &[("slirp", NatBackend::Slirp), ("passt", NatBackend::Passt)],
                )?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "network.port_forwards",
            get: |cfg| {
                cfg.network
                    .port_forwards
                    .iter()
                    .map(|f| {
                        format!(
                            "{}:{}->{}",
                            match f.protocol {
                                PortForwardProtocol::Tcp => "tcp",
                                PortForwardProtocol::Udp => "udp",
                            },
                            f.host_port,
                            f.guest_port
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            },
            set: None,
            immutable_reason: Some("port forwards are fixed at create time (QEMU netdev hostfwd)"),
            live: false,
        },
        ConfigKey {
            key: "audio.backend",
            get: |cfg| enum_lower(&cfg.audio.backend),
            set: Some(|cfg, value| {
                cfg.audio.backend = parse_enum(
                    value,
                    &[
                        ("pipewire", AudioBackend::Pipewire),
                        ("pulseaudio", AudioBackend::Pulseaudio),
                        ("none", AudioBackend::None),
                    ],
                )?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "audio.device",
            get: |cfg| enum_lower(&cfg.audio.device),
            set: Some(|cfg, value| {
                cfg.audio.device = parse_enum(
                    value,
                    &[
                        ("virtiosound", AudioDevice::VirtioSound),
                        ("ich9hda", AudioDevice::Ich9Hda),
                    ],
                )?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "input.pointer_mode",
            get: |cfg| enum_lower(&cfg.input.pointer_mode),
            set: Some(|cfg, value| {
                cfg.input.pointer_mode = parse_enum(
                    value,
                    &[
                        ("tablet", PointerMode::Tablet),
                        ("mouse", PointerMode::Mouse),
                    ],
                )?;
                Ok(())
            }),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "input.hide_host_cursor",
            get: input_hide_host_cursor_get,
            set: Some(input_hide_host_cursor_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "input.clipboard_enabled",
            get: input_clipboard_enabled_get,
            set: Some(input_clipboard_enabled_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "firmware.enable_uefi",
            get: firmware_enable_uefi_get,
            set: Some(firmware_enable_uefi_set),
            immutable_reason: None,
            live: false,
        },
        ConfigKey {
            key: "firmware.ovmf_code_path",
            get: |cfg| cfg.firmware.ovmf_code_path.display().to_string(),
            set: None,
            immutable_reason: Some("set via ANDLERD_OVMF_CODE or firmware discovery"),
            live: false,
        },
        ConfigKey {
            key: "firmware.ovmf_vars_path",
            get: |cfg| cfg.firmware.ovmf_vars_path.display().to_string(),
            set: None,
            immutable_reason: Some("VARS.fd belongs to the instance directory"),
            live: false,
        },
        ConfigKey {
            key: "kind.android_profile.arm_translator",
            get: android_arm_translator_get,
            set: None,
            immutable_reason: Some(
                "use the dedicated arm-translator command; switching rebuilds the image",
            ),
            live: false,
        },
        ConfigKey {
            key: "kind.android_profile.boot_mode",
            get: android_boot_mode_get,
            set: None,
            immutable_reason: Some(
                "use the dedicated boot-mode command; switching rewrites the guest image",
            ),
            live: false,
        },
        ConfigKey {
            key: "kind.android_profile.android_version",
            get: android_version_get,
            set: None,
            immutable_reason: Some("the Android version is fixed at creation"),
            live: false,
        },
        ConfigKey {
            key: "extra_disks",
            get: extra_disks_get,
            set: None,
            immutable_reason: Some("use `andler attach disk` / `andler detach disk`"),
            live: false,
        },
        ConfigKey {
            key: "extra_networks",
            get: extra_networks_get,
            set: None,
            immutable_reason: Some("use `andler attach net` / `andler detach net`"),
            live: false,
        },
    ];
    KEYS
}

/// Returns the canonical string form of a key's value in `cfg`.
pub fn get_key(cfg: &InstanceConfig, key: &str) -> Option<String> {
    config_keys()
        .iter()
        .find(|row| row.key == key)
        .map(|row| (row.get)(cfg))
}

/// Applies `value` to `key` in `cfg`, in place. Mutable keys parse and
/// assign; immutable keys and unknown keys return an explicit error.
pub fn set_key(cfg: &mut InstanceConfig, key: &str, value: &str) -> Result<(), ConfigKeyError> {
    let row = config_keys()
        .iter()
        .find(|row| row.key == key)
        .ok_or_else(|| ConfigKeyError::Unknown(key.to_string()))?;

    let Some(setter) = row.set else {
        return Err(ConfigKeyError::Immutable {
            key: key.to_string(),
            reason: row.immutable_reason.unwrap_or("read-only key"),
        });
    };

    setter(cfg, value).map_err(|reason| ConfigKeyError::InvalidValue {
        key: key.to_string(),
        value: value.to_string(),
        reason,
    })
}

/// Lists `(key, from_value, to_value)` triples where the two configs differ
/// in canonical form. Used by `config status` to show file-vs-memory drift.
pub fn diff_configs(from: &InstanceConfig, to: &InstanceConfig) -> Vec<(String, String, String)> {
    config_keys()
        .iter()
        .filter_map(|row| {
            let from_value = (row.get)(from);
            let to_value = (row.get)(to);
            (from_value != to_value).then(|| (row.key.to_string(), from_value, to_value))
        })
        .collect()
}

/// True when a key can be applied to a running guest without a reboot.
pub fn is_live_key(key: &str) -> bool {
    config_keys()
        .iter()
        .find(|row| row.key == key)
        .map(|row| row.live)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_profile::{AndroidBootMode, AndroidProfile, AndroidVersion, ArmTranslator};
    use crate::config::{
        CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig, InputConfig,
        InstanceId, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: crate::config::BackendKind::Qemu,
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/firmware-VARS.fd")),
            audio: crate::config::AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    fn android_config() -> InstanceConfig {
        let mut cfg = sample_config();
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: true,
                microg: false,
                arm_translator: ArmTranslator::Libndk,
                boot_mode: AndroidBootMode::Android,
                base_image_pin: None,
            },
        };
        cfg
    }

    #[test]
    fn mutable_keys_round_trip_through_get() {
        let mut cfg = sample_config();
        for key in ["cpu.cores", "memory.size_bytes", "display.resolution"] {
            let value = get_key(&cfg, key).unwrap();
            set_key(&mut cfg, key, &value).unwrap();
            assert_eq!(get_key(&cfg, key).unwrap(), value, "key {key}");
        }
    }

    #[test]
    fn set_key_parses_human_values() {
        let mut cfg = sample_config();
        set_key(&mut cfg, "memory.size_bytes", "8G").unwrap();
        assert_eq!(cfg.memory.size_bytes, 8 * 1024 * 1024 * 1024);
        set_key(&mut cfg, "display.resolution", "1920x1080").unwrap();
        assert_eq!(cfg.display.resolution.width, 1920);
        set_key(&mut cfg, "input.pointer_mode", "MOUSE").unwrap();
        assert_eq!(cfg.input.pointer_mode, PointerMode::Mouse);
        set_key(&mut cfg, "network.mode", "bridge:br0").unwrap();
        assert_eq!(
            cfg.network.mode,
            NetworkMode::Bridge {
                interface: "br0".to_string()
            }
        );
        set_key(&mut cfg, "gpu.render_backend", "passthrough:0000:01:00.0").unwrap();
        assert!(matches!(
            cfg.gpu.render_backend,
            RenderBackend::Passthrough { .. }
        ));
    }

    #[test]
    fn set_key_rejects_bad_values_with_reason() {
        let mut cfg = sample_config();
        let err = set_key(&mut cfg, "cpu.cores", "many").unwrap_err();
        assert!(matches!(err, ConfigKeyError::InvalidValue { .. }));
        let err = set_key(&mut cfg, "memory.size_bytes", "1.5GB").unwrap_err();
        assert!(matches!(err, ConfigKeyError::InvalidValue { .. }));
    }

    #[test]
    fn immutable_keys_are_rejected_with_reason() {
        let mut cfg = sample_config();
        for row in config_keys().iter().filter(|row| row.set.is_none()) {
            let err = set_key(&mut cfg, row.key, "whatever").unwrap_err();
            assert!(
                matches!(err, ConfigKeyError::Immutable { .. }),
                "key {} should be immutable",
                row.key
            );
        }
    }

    #[test]
    fn unknown_key_is_rejected() {
        let mut cfg = sample_config();
        assert!(matches!(
            set_key(&mut cfg, "no.such.key", "1").unwrap_err(),
            ConfigKeyError::Unknown(_)
        ));
    }

    #[test]
    fn display_resolution_is_live_key() {
        assert!(is_live_key("display.resolution"));
        assert!(!is_live_key("cpu.cores"));
    }

    #[test]
    fn diff_configs_reports_only_changed_keys() {
        let mut cfg = sample_config();
        let changed = cfg.clone();
        set_key(&mut cfg, "name", "renamed").unwrap();
        set_key(&mut cfg, "display.resolution", "1280x720").unwrap();
        let diff = diff_configs(&changed, &cfg);
        let keys: Vec<&str> = diff.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["name", "display.resolution"]);
    }

    #[test]
    fn android_keys_are_empty_on_linux_vm() {
        let linux = sample_config();
        assert_eq!(
            get_key(&linux, "kind.android_profile.arm_translator").unwrap(),
            ""
        );
        let android = android_config();
        assert_eq!(
            get_key(&android, "kind.android_profile.arm_translator").unwrap(),
            "libndk"
        );
    }

    #[test]
    fn every_mutable_key_set_leaves_config_valid() {
        // Mutating keys must never produce a config that fails validation on
        // its own (range checks happen in `validate`; parse errors here).
        for row in config_keys().iter().filter(|row| row.set.is_some()) {
            let mut cfg = sample_config();
            let current = (row.get)(&cfg);
            set_key(&mut cfg, row.key, &current).unwrap_or_else(|err| {
                panic!("re-setting current value of {} failed: {err}", row.key)
            });
        }
    }

    /// Guards against this table silently drifting out of sync with
    /// `InstanceConfig` — the exact failure mode this table replaced
    /// (`config set`'s old 3-key whitelist was a partial, hand-maintained
    /// copy of the schema that fields quietly fell through). Walks a real
    /// `InstanceConfig` via its own `Serialize` impl instead of hand-listing
    /// fields a second time, so it can't itself drift the same way: add a
    /// field to any nested `*Config` struct without a matching row here and
    /// this test names exactly which one is missing, instead of the field
    /// being unreachable through `config set`/`config status` forever.
    #[test]
    fn every_leaf_field_has_a_config_key_row() {
        let cfg = sample_config();
        let value = serde_json::to_value(&cfg).expect("InstanceConfig must serialize");
        let object = value
            .as_object()
            .expect("InstanceConfig serializes to an object");

        let known_keys: std::collections::HashSet<&str> =
            config_keys().iter().map(|row| row.key).collect();

        // Top-level fields that aren't part of the generic per-section walk
        // below, checked by name instead: `kind` is a tagged enum (its own
        // variant fields are covered separately, see `kind.android_profile.*`
        // above), `extra_disks`/`extra_networks` are collections — neither
        // reduces to a flat scalar leaf the way a `*Config` struct's fields
        // do, so each gets exactly one opaque row instead.
        let opaque_top_level = [
            "id",
            "name",
            "kind",
            "backend",
            "schema_version",
            "extra_disks",
            "extra_networks",
        ];
        for field in opaque_top_level {
            assert!(
                known_keys.contains(field),
                "InstanceConfig.{field} has no config_keys() row"
            );
        }

        // Every other top-level field is a nested `*Config` struct: walk its
        // serialized leaves generically and require a `section.leaf` row for
        // each one (settable, or immutable with a reason — either is fine,
        // silently missing is not).
        let nested_sections = [
            "cpu", "memory", "disk", "display", "gpu", "network", "audio", "input", "firmware",
        ];
        for section in nested_sections {
            let section_object = object
                .get(section)
                .unwrap_or_else(|| panic!("InstanceConfig has no `{section}` field"))
                .as_object()
                .unwrap_or_else(|| panic!("`{section}` did not serialize to a JSON object"));
            for leaf in section_object.keys() {
                let dotted = format!("{section}.{leaf}");
                assert!(
                    known_keys.contains(dotted.as_str()),
                    "InstanceConfig.{dotted} has no config_keys() row in keypath.rs — add \
                     one (settable, or immutable with a reason)"
                );
            }
        }

        // Closes the loop between the two checks above: every top-level
        // field must be in exactly one of `opaque_top_level` or
        // `nested_sections`. A brand new top-level field on InstanceConfig
        // would otherwise fall through both and go unnoticed here too.
        let accounted_for: std::collections::HashSet<&str> = opaque_top_level
            .iter()
            .copied()
            .chain(nested_sections.iter().copied())
            .collect();
        for field in object.keys() {
            assert!(
                accounted_for.contains(field.as_str()),
                "InstanceConfig gained a new top-level field `{field}` this test doesn't \
                 know about — add it to `opaque_top_level` or `nested_sections` above, and \
                 give it a config_keys() row"
            );
        }
    }
}
