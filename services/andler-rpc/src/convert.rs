//! Конвертации между сгенерированными `proto`-типами и доменными типами
//! `andler-core`. Каждое направление явное (`From`/`TryFrom`), без `serde`
//! на proto-типах — это намеренно ручной, видимый код, а не магия
//! derive-макроса поверх protobuf-сообщений.

use std::path::PathBuf;

use crate::proto;
use andler_core::{
    AndroidProfile, AndroidVersion, ArmTranslator, AudioBackend, AudioConfig, AudioDevice,
    BackendKind, CdromBus, CloneMode, CpuConfig, CpuPriority, DiskConfig, DiskFormat,
    DisplayConfig, DisplayEngine, FirmwareConfig, GpuConfig, InputConfig, InstanceConfig,
    InstanceId, InstanceKind, InstanceState, LogLine, LogStreamSource, MemoryConfig, NatBackend,
    NetworkConfig, NetworkMode, PointerMode, RenderBackend, Resolution, ResourceMetrics, RootMode,
};

/// Ошибка конвертации proto-сообщения в доменный тип — на практике сейчас
/// только "пришло значение enum'а, для которого нет соответствия"
/// (`ANDROID_VERSION_UNSPECIFIED`/`ROOT_MODE_UNSPECIFIED`, либо вообще не
/// входящее в диапазон известных `prost` значение).
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("missing or unspecified android_version")]
    MissingAndroidVersion,
    #[error("missing or unspecified root_mode")]
    MissingRootMode,
    #[error("missing instance_id")]
    MissingInstanceId,
    #[error("invalid instance_id {0:?}: {1}")]
    InvalidInstanceId(String, uuid::Error),
    #[error("missing or unspecified cpu_priority")]
    MissingCpuPriority,
    #[error("missing or unspecified disk_format")]
    MissingDiskFormat,
    #[error("missing or unspecified display_engine")]
    MissingDisplayEngine,
    #[error("missing or unspecified audio_backend")]
    MissingAudioBackend,
    #[error("missing render_backend.kind oneof")]
    MissingRenderBackendKind,
    #[error("missing network_mode.kind oneof")]
    MissingNetworkModeKind,
    #[error("missing or unspecified clone_mode")]
    MissingCloneMode,
    #[error("missing field `{0}` in request")]
    MissingField(&'static str),
}

impl TryFrom<proto::AndroidVersion> for AndroidVersion {
    type Error = ConvertError;

    fn try_from(value: proto::AndroidVersion) -> Result<Self, Self::Error> {
        match value {
            proto::AndroidVersion::Android11 => Ok(AndroidVersion::Android11),
            proto::AndroidVersion::Android13 => Ok(AndroidVersion::Android13),
            proto::AndroidVersion::Unspecified => Err(ConvertError::MissingAndroidVersion),
        }
    }
}

impl From<AndroidVersion> for proto::AndroidVersion {
    fn from(value: AndroidVersion) -> Self {
        match value {
            AndroidVersion::Android11 => proto::AndroidVersion::Android11,
            AndroidVersion::Android13 => proto::AndroidVersion::Android13,
        }
    }
}

impl TryFrom<proto::CloneMode> for CloneMode {
    type Error = ConvertError;

    fn try_from(value: proto::CloneMode) -> Result<Self, Self::Error> {
        match value {
            proto::CloneMode::Linked => Ok(CloneMode::Linked),
            proto::CloneMode::FullStandalone => Ok(CloneMode::FullStandalone),
            proto::CloneMode::SharedBase => Ok(CloneMode::SharedBase),
            proto::CloneMode::Unspecified => Err(ConvertError::MissingCloneMode),
        }
    }
}

impl TryFrom<proto::RootMode> for RootMode {
    type Error = ConvertError;

    fn try_from(value: proto::RootMode) -> Result<Self, Self::Error> {
        match value {
            proto::RootMode::None => Ok(RootMode::None),
            proto::RootMode::Magisk => Ok(RootMode::Magisk),
            proto::RootMode::Unspecified => Err(ConvertError::MissingRootMode),
        }
    }
}

impl From<RootMode> for proto::RootMode {
    fn from(value: RootMode) -> Self {
        match value {
            RootMode::None => proto::RootMode::None,
            RootMode::Magisk => proto::RootMode::Magisk,
        }
    }
}

/// `UNSPECIFIED` -> `ArmTranslator::None` — не ошибка: старые клиенты,
/// которые не знали об этом поле, физически не имели транслятора, так
/// что это точное, а не приблизительное значение по умолчанию.
impl From<proto::ArmTranslator> for ArmTranslator {
    fn from(value: proto::ArmTranslator) -> Self {
        match value {
            proto::ArmTranslator::Libndk => ArmTranslator::Libndk,
            proto::ArmTranslator::Libhoudini => ArmTranslator::Libhoudini,
            proto::ArmTranslator::ArmTranslatorNone | proto::ArmTranslator::Unspecified => {
                ArmTranslator::None
            }
        }
    }
}

impl From<ArmTranslator> for proto::ArmTranslator {
    fn from(value: ArmTranslator) -> Self {
        match value {
            ArmTranslator::None => proto::ArmTranslator::ArmTranslatorNone,
            ArmTranslator::Libndk => proto::ArmTranslator::Libndk,
            ArmTranslator::Libhoudini => proto::ArmTranslator::Libhoudini,
        }
    }
}

impl TryFrom<proto::AndroidProfile> for AndroidProfile {
    type Error = ConvertError;

    fn try_from(value: proto::AndroidProfile) -> Result<Self, Self::Error> {
        Ok(AndroidProfile {
            android_version: value.android_version().try_into()?,
            gapps: value.gapps,
            microg: value.microg,
            arm_translator: value.arm_translator().into(),
            root: value.root().try_into()?,
        })
    }
}

impl From<AndroidProfile> for proto::AndroidProfile {
    fn from(value: AndroidProfile) -> Self {
        let mut msg = proto::AndroidProfile {
            gapps: value.gapps,
            microg: value.microg,
            ..Default::default()
        };
        msg.set_android_version(value.android_version.into());
        msg.set_root(value.root.into());
        msg.set_arm_translator(value.arm_translator.into());
        msg
    }
}

// --- InstanceConfig (CreateInstanceRequest, LinuxVm) --------------------
//
// Каждая пара impl здесь — зеркало одного типа из `andler_core::config`.
// Направление proto -> domain — `TryFrom` (enum'ы/oneof могут прийти
// `UNSPECIFIED`/отсутствующими), domain -> proto — всегда `From` (доменные
// типы по построению полны, конвертация в proto не может провалиться).
// domain -> proto не используется в `service.rs` сегодня (там только
// proto -> domain для `CreateInstanceRequest`), но добавлен симметрично
// остальным типам в этом файле (`AndroidProfile`) — если `andler-cli`
// когда-нибудь захочет показать уже созданный `InstanceConfig` обратно
// через proto (например, "показать текущую конфигурацию инстанса"), эта
// сторона уже на месте, а не специально вырезана.

impl TryFrom<proto::CpuPriority> for CpuPriority {
    type Error = ConvertError;

    fn try_from(value: proto::CpuPriority) -> Result<Self, Self::Error> {
        match value {
            proto::CpuPriority::Low => Ok(CpuPriority::Low),
            proto::CpuPriority::Normal => Ok(CpuPriority::Normal),
            proto::CpuPriority::High => Ok(CpuPriority::High),
            proto::CpuPriority::Unspecified => Err(ConvertError::MissingCpuPriority),
        }
    }
}

impl From<CpuPriority> for proto::CpuPriority {
    fn from(value: CpuPriority) -> Self {
        match value {
            CpuPriority::Low => proto::CpuPriority::Low,
            CpuPriority::Normal => proto::CpuPriority::Normal,
            CpuPriority::High => proto::CpuPriority::High,
        }
    }
}

impl TryFrom<proto::CpuConfig> for CpuConfig {
    type Error = ConvertError;

    fn try_from(value: proto::CpuConfig) -> Result<Self, Self::Error> {
        // `priority()` — геттер на `&self` (`prost`-сгенерированный, для
        // удобного доступа к enum-полю без ручного `i32`-каста), но
        // вызывается после того, как `value.affinity` уже могло быть
        // перемещено через `into_iter()` ниже — значит, его нужно
        // прочитать первым, пока `value` ещё не тронут по частям.
        let priority = value.priority().try_into()?;

        Ok(CpuConfig {
            cores: value.cores,
            sockets: value.sockets,
            threads: value.threads,
            // Пустой `repeated` ⇒ `None` — см. комментарий у поля
            // `affinity` в `andler.proto`.
            affinity: if value.affinity.is_empty() {
                None
            } else {
                Some(value.affinity.into_iter().map(|v| v as usize).collect())
            },
            priority,
        })
    }
}

impl From<CpuConfig> for proto::CpuConfig {
    fn from(value: CpuConfig) -> Self {
        let mut msg = proto::CpuConfig {
            cores: value.cores,
            sockets: value.sockets,
            threads: value.threads,
            affinity: value
                .affinity
                .unwrap_or_default()
                .into_iter()
                .map(|v| v as u64)
                .collect(),
            ..Default::default()
        };
        msg.set_priority(value.priority.into());
        msg
    }
}

impl From<proto::MemoryConfig> for MemoryConfig {
    fn from(value: proto::MemoryConfig) -> Self {
        MemoryConfig {
            size_bytes: value.size_bytes,
            ballooning: value.ballooning,
            zram: value.zram,
            ksm: value.ksm,
        }
    }
}

impl From<MemoryConfig> for proto::MemoryConfig {
    fn from(value: MemoryConfig) -> Self {
        proto::MemoryConfig {
            size_bytes: value.size_bytes,
            ballooning: value.ballooning,
            zram: value.zram,
            ksm: value.ksm,
        }
    }
}

impl TryFrom<proto::DiskFormat> for DiskFormat {
    type Error = ConvertError;

    fn try_from(value: proto::DiskFormat) -> Result<Self, Self::Error> {
        match value {
            proto::DiskFormat::Qcow2 => Ok(DiskFormat::Qcow2),
            proto::DiskFormat::Raw => Ok(DiskFormat::Raw),
            proto::DiskFormat::Vdi => Ok(DiskFormat::Vdi),
            proto::DiskFormat::Unspecified => Err(ConvertError::MissingDiskFormat),
        }
    }
}

impl From<DiskFormat> for proto::DiskFormat {
    fn from(value: DiskFormat) -> Self {
        match value {
            DiskFormat::Qcow2 => proto::DiskFormat::Qcow2,
            DiskFormat::Raw => proto::DiskFormat::Raw,
            DiskFormat::Vdi => proto::DiskFormat::Vdi,
        }
    }
}

impl From<proto::CdromBus> for CdromBus {
    /// `CDROM_BUS_UNSPECIFIED` → `CdromBus::default()` (`Ide`), не ошибка
    /// — в отличие от `DiskFormat`. См. doc-комментарий
    /// `enum CdromBus` в `andler.proto`.
    fn from(value: proto::CdromBus) -> Self {
        match value {
            proto::CdromBus::VirtioScsi => CdromBus::VirtioScsi,
            proto::CdromBus::Ide => CdromBus::Ide,
            proto::CdromBus::Unspecified => CdromBus::default(),
        }
    }
}

impl From<CdromBus> for proto::CdromBus {
    fn from(value: CdromBus) -> Self {
        match value {
            CdromBus::VirtioScsi => proto::CdromBus::VirtioScsi,
            CdromBus::Ide => proto::CdromBus::Ide,
        }
    }
}

impl TryFrom<proto::DiskConfig> for DiskConfig {
    type Error = ConvertError;

    fn try_from(value: proto::DiskConfig) -> Result<Self, Self::Error> {
        // Тот же порядок, что в `TryFrom<proto::CpuConfig>` выше:
        // `format()` (геттер на `&self`) читается до того, как
        // `value.path`/`value.base_image` перемещаются по значению.
        let format = value.format().try_into()?;

        Ok(DiskConfig {
            path: PathBuf::from(value.path),
            size_bytes: value.size_bytes,
            format,
            // Пустая строка ⇒ `None` — см. комментарий у поля
            // `base_image` в `andler.proto`.
            base_image: if value.base_image.is_empty() {
                None
            } else {
                Some(PathBuf::from(value.base_image))
            },
            thin_provisioning: value.thin_provisioning,
            trim_on_shutdown: value.trim_on_shutdown,
            compact_on_shutdown: value.compact_on_shutdown,
            snapshot_timeout_secs: value.snapshot_timeout_secs,
        })
    }
}

impl From<DiskConfig> for proto::DiskConfig {
    fn from(value: DiskConfig) -> Self {
        let mut msg = proto::DiskConfig {
            path: value.path.to_string_lossy().into_owned(),
            size_bytes: value.size_bytes,
            base_image: value
                .base_image
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            thin_provisioning: value.thin_provisioning,
            trim_on_shutdown: value.trim_on_shutdown,
            compact_on_shutdown: value.compact_on_shutdown,
            snapshot_timeout_secs: value.snapshot_timeout_secs,
            ..Default::default()
        };
        msg.set_format(value.format.into());
        msg
    }
}

impl From<proto::Resolution> for Resolution {
    fn from(value: proto::Resolution) -> Self {
        Resolution::new(value.width, value.height)
    }
}

impl From<Resolution> for proto::Resolution {
    fn from(value: Resolution) -> Self {
        proto::Resolution {
            width: value.width,
            height: value.height,
        }
    }
}

impl TryFrom<proto::DisplayEngine> for DisplayEngine {
    type Error = ConvertError;

    fn try_from(value: proto::DisplayEngine) -> Result<Self, Self::Error> {
        match value {
            proto::DisplayEngine::Sdl => Ok(DisplayEngine::Sdl),
            proto::DisplayEngine::Spice => Ok(DisplayEngine::Spice),
            proto::DisplayEngine::Dbus => Ok(DisplayEngine::Dbus),
            // `DisplayNone`, не `None` — `DISPLAY_NONE` в proto не делится
            // префиксом `DISPLAY_ENGINE_` с `DisplayEngineUnspecified`, и
            // `prost` поэтому не обрезает префикс (тот же эффект, что у
            // `AudioBackend::AudioNone` для `AUDIO_NONE` ниже).
            proto::DisplayEngine::DisplayNone => Ok(DisplayEngine::None),
            proto::DisplayEngine::Gtk => Ok(DisplayEngine::Gtk),
            proto::DisplayEngine::Unspecified => Err(ConvertError::MissingDisplayEngine),
        }
    }
}

impl From<DisplayEngine> for proto::DisplayEngine {
    fn from(value: DisplayEngine) -> Self {
        match value {
            DisplayEngine::Sdl => proto::DisplayEngine::Sdl,
            DisplayEngine::Spice => proto::DisplayEngine::Spice,
            DisplayEngine::Dbus => proto::DisplayEngine::Dbus,
            DisplayEngine::None => proto::DisplayEngine::DisplayNone,
            DisplayEngine::Gtk => proto::DisplayEngine::Gtk,
        }
    }
}

impl TryFrom<proto::DisplayConfig> for DisplayConfig {
    type Error = ConvertError;

    fn try_from(value: proto::DisplayConfig) -> Result<Self, Self::Error> {
        // Тот же порядок, что в `CpuConfig`/`DiskConfig` выше:
        // `display_engine()` — геттер на `&self`, значит читается до
        // того, как `value.resolution` перемещается по значению через
        // `.ok_or(...)?` ниже (иначе `value` уже частично moved, и
        // `&self`-метод на нём не скомпилируется).
        let display_engine = value.display_engine().try_into()?;
        let resolution = value
            .resolution
            .ok_or(ConvertError::MissingField("display.resolution"))?
            .into();

        Ok(DisplayConfig {
            resolution,
            dpi: value.dpi,
            fps_limit: value.fps_limit,
            display_engine,
            fullscreen: value.fullscreen,
        })
    }
}

impl From<DisplayConfig> for proto::DisplayConfig {
    fn from(value: DisplayConfig) -> Self {
        let mut msg = proto::DisplayConfig {
            resolution: Some(value.resolution.into()),
            dpi: value.dpi,
            fps_limit: value.fps_limit,
            fullscreen: value.fullscreen,
            ..Default::default()
        };
        msg.set_display_engine(value.display_engine.into());
        msg
    }
}

impl TryFrom<proto::RenderBackend> for RenderBackend {
    type Error = ConvertError;

    fn try_from(value: proto::RenderBackend) -> Result<Self, Self::Error> {
        use proto::render_backend::Kind;

        match value.kind.ok_or(ConvertError::MissingRenderBackendKind)? {
            Kind::Venus(_) => Ok(RenderBackend::Venus),
            Kind::VirtioGpu(_) => Ok(RenderBackend::VirtioGpu),
            Kind::VirGl(_) => Ok(RenderBackend::VirGl),
            Kind::Cpu(_) => Ok(RenderBackend::Cpu),
            Kind::Passthrough(p) => Ok(RenderBackend::Passthrough {
                gpu_pci_id: p.gpu_pci_id,
            }),
        }
    }
}

impl From<RenderBackend> for proto::RenderBackend {
    fn from(value: RenderBackend) -> Self {
        use proto::render_backend::Kind;

        let kind = match value {
            RenderBackend::Venus => Kind::Venus(proto::render_backend::Venus {}),
            RenderBackend::VirtioGpu => Kind::VirtioGpu(proto::render_backend::VirtioGpu {}),
            RenderBackend::VirGl => Kind::VirGl(proto::render_backend::VirGl {}),
            RenderBackend::Cpu => Kind::Cpu(proto::render_backend::Cpu {}),
            RenderBackend::Passthrough { gpu_pci_id } => {
                Kind::Passthrough(proto::render_backend::Passthrough { gpu_pci_id })
            }
        };
        proto::RenderBackend { kind: Some(kind) }
    }
}

impl TryFrom<proto::GpuConfig> for GpuConfig {
    type Error = ConvertError;

    fn try_from(value: proto::GpuConfig) -> Result<Self, Self::Error> {
        let render_backend = value
            .render_backend
            .ok_or(ConvertError::MissingField("gpu.render_backend"))?
            .try_into()?;

        Ok(GpuConfig {
            render_backend,
            hostmem_bytes: value.hostmem_bytes,
            blob: value.blob,
            gl: value.gl,
        })
    }
}

impl From<GpuConfig> for proto::GpuConfig {
    fn from(value: GpuConfig) -> Self {
        proto::GpuConfig {
            render_backend: Some(value.render_backend.into()),
            hostmem_bytes: value.hostmem_bytes,
            blob: value.blob,
            gl: value.gl,
        }
    }
}

impl TryFrom<proto::NetworkMode> for NetworkMode {
    type Error = ConvertError;

    fn try_from(value: proto::NetworkMode) -> Result<Self, Self::Error> {
        use proto::network_mode::Kind;

        match value.kind.ok_or(ConvertError::MissingNetworkModeKind)? {
            Kind::Nat(_) => Ok(NetworkMode::Nat),
            Kind::Bridge(b) => Ok(NetworkMode::Bridge {
                interface: b.interface,
            }),
            Kind::Isolated(_) => Ok(NetworkMode::Isolated),
        }
    }
}

impl From<NetworkMode> for proto::NetworkMode {
    fn from(value: NetworkMode) -> Self {
        use proto::network_mode::Kind;

        let kind = match value {
            NetworkMode::Nat => Kind::Nat(proto::network_mode::Nat {}),
            NetworkMode::Bridge { interface } => {
                Kind::Bridge(proto::network_mode::Bridge { interface })
            }
            NetworkMode::Isolated => Kind::Isolated(proto::network_mode::Isolated {}),
        };
        proto::NetworkMode { kind: Some(kind) }
    }
}

/// `UNSPECIFIED` -> `Slirp` — не ошибка, а фоллбэк на поведение `start.sh`,
/// т.к. старые сохранённые конфиги (до появления `nat_backend`) физически
/// не могли заполнить это поле (тот же приём, что для `CdromBus`).
impl From<proto::NatBackend> for NatBackend {
    fn from(value: proto::NatBackend) -> Self {
        match value {
            proto::NatBackend::Passt => NatBackend::Passt,
            proto::NatBackend::Slirp | proto::NatBackend::Unspecified => NatBackend::Slirp,
        }
    }
}

impl From<NatBackend> for proto::NatBackend {
    fn from(value: NatBackend) -> Self {
        match value {
            NatBackend::Slirp => proto::NatBackend::Slirp,
            NatBackend::Passt => proto::NatBackend::Passt,
        }
    }
}

impl TryFrom<proto::NetworkConfig> for NetworkConfig {
    type Error = ConvertError;

    fn try_from(value: proto::NetworkConfig) -> Result<Self, Self::Error> {
        let mode = value
            .mode
            .ok_or(ConvertError::MissingField("network.mode"))?
            .try_into()?;
        let nat_backend = value.nat_backend().into();

        Ok(NetworkConfig {
            mode,
            device_model: value.device_model,
            nat_backend,
        })
    }
}

impl From<NetworkConfig> for proto::NetworkConfig {
    fn from(value: NetworkConfig) -> Self {
        let mut msg = proto::NetworkConfig {
            mode: Some(value.mode.into()),
            device_model: value.device_model,
            ..Default::default()
        };
        msg.set_nat_backend(value.nat_backend.into());
        msg
    }
}

impl From<proto::FirmwareConfig> for FirmwareConfig {
    fn from(value: proto::FirmwareConfig) -> Self {
        FirmwareConfig {
            ovmf_code_path: PathBuf::from(value.ovmf_code_path),
            ovmf_vars_path: PathBuf::from(value.ovmf_vars_path),
        }
    }
}

impl From<FirmwareConfig> for proto::FirmwareConfig {
    fn from(value: FirmwareConfig) -> Self {
        proto::FirmwareConfig {
            ovmf_code_path: value.ovmf_code_path.to_string_lossy().into_owned(),
            ovmf_vars_path: value.ovmf_vars_path.to_string_lossy().into_owned(),
        }
    }
}

impl TryFrom<proto::AudioBackend> for AudioBackend {
    type Error = ConvertError;

    fn try_from(value: proto::AudioBackend) -> Result<Self, Self::Error> {
        match value {
            proto::AudioBackend::Pipewire => Ok(AudioBackend::Pipewire),
            proto::AudioBackend::Pulseaudio => Ok(AudioBackend::Pulseaudio),
            proto::AudioBackend::AudioNone => Ok(AudioBackend::None),
            proto::AudioBackend::Unspecified => Err(ConvertError::MissingAudioBackend),
        }
    }
}

impl From<AudioBackend> for proto::AudioBackend {
    fn from(value: AudioBackend) -> Self {
        match value {
            AudioBackend::Pipewire => proto::AudioBackend::Pipewire,
            AudioBackend::Pulseaudio => proto::AudioBackend::Pulseaudio,
            AudioBackend::None => proto::AudioBackend::AudioNone,
        }
    }
}

/// `UNSPECIFIED` -> `VirtioSound` — не ошибка, тот же фоллбэк-приём, что
/// для `NatBackend`/`CdromBus`: старые сохранённые конфиги физически не
/// могли заполнить это поле.
impl From<proto::AudioDevice> for AudioDevice {
    fn from(value: proto::AudioDevice) -> Self {
        match value {
            proto::AudioDevice::Ich9Hda => AudioDevice::Ich9Hda,
            proto::AudioDevice::VirtioSound | proto::AudioDevice::Unspecified => {
                AudioDevice::VirtioSound
            }
        }
    }
}

impl From<AudioDevice> for proto::AudioDevice {
    fn from(value: AudioDevice) -> Self {
        match value {
            AudioDevice::VirtioSound => proto::AudioDevice::VirtioSound,
            AudioDevice::Ich9Hda => proto::AudioDevice::Ich9Hda,
        }
    }
}

impl TryFrom<proto::AudioConfig> for AudioConfig {
    type Error = ConvertError;

    fn try_from(value: proto::AudioConfig) -> Result<Self, Self::Error> {
        Ok(AudioConfig {
            backend: value.backend().try_into()?,
            device: value.device().into(),
        })
    }
}

impl From<AudioConfig> for proto::AudioConfig {
    fn from(value: AudioConfig) -> Self {
        let mut msg = proto::AudioConfig::default();
        msg.set_backend(value.backend.into());
        msg.set_device(value.device.into());
        msg
    }
}

/// `UNSPECIFIED` -> фоллбэк на устаревшее поле `tablet_mode` (обратная
/// совместимость со старыми клиентами) -> `Tablet` по умолчанию, если и
/// оно отсутствует (proto3 `bool` по умолчанию `false`, что дало бы
/// `Mouse` — поэтому явный `Unspecified` fallback на `tablet_mode`, а не
/// слепое чтение bool).
impl From<proto::InputConfig> for InputConfig {
    fn from(value: proto::InputConfig) -> Self {
        let pointer_mode = match value.pointer_mode() {
            proto::PointerMode::Tablet => PointerMode::Tablet,
            proto::PointerMode::Mouse => PointerMode::Mouse,
            proto::PointerMode::Unspecified => {
                if value.tablet_mode {
                    PointerMode::Tablet
                } else {
                    PointerMode::Mouse
                }
            }
        };
        InputConfig {
            pointer_mode,
            hide_host_cursor: value.hide_host_cursor,
            clipboard_enabled: value.clipboard_enabled,
        }
    }
}

impl From<InputConfig> for proto::InputConfig {
    fn from(value: InputConfig) -> Self {
        let mut msg = proto::InputConfig {
            // Заполняем и устаревшее поле — старые клиенты, которые ещё
            // не знают про `pointer_mode`, продолжают работать.
            tablet_mode: value.pointer_mode == PointerMode::Tablet,
            hide_host_cursor: value.hide_host_cursor,
            clipboard_enabled: value.clipboard_enabled,
            ..Default::default()
        };
        msg.set_pointer_mode(value.pointer_mode.into());
        msg
    }
}

impl From<PointerMode> for proto::PointerMode {
    fn from(value: PointerMode) -> Self {
        match value {
            PointerMode::Tablet => proto::PointerMode::Tablet,
            PointerMode::Mouse => proto::PointerMode::Mouse,
        }
    }
}

/// Собирает полный `InstanceConfig` (`InstanceKind::LinuxVm`) из
/// `CreateInstanceRequest`. `id` генерируется здесь же
/// (`InstanceId::new()`), а не получается из запроса — клиент не выбирает
/// `InstanceId`, как и в `create_android_instance` (см. комментарий у
/// `rpc CreateInstance` в `andler.proto`). `backend` всегда
/// `BackendKind::Qemu` — единственный зарегистрированный backend сегодня.
impl TryFrom<proto::CreateInstanceRequest> for InstanceConfig {
    type Error = ConvertError;

    fn try_from(value: proto::CreateInstanceRequest) -> Result<Self, Self::Error> {
        Ok(InstanceConfig {
            id: InstanceId::new(),
            name: value.name,
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from(value.iso_path),
                cdrom_bus: value.cdrom_bus().into(),
            },
            backend: BackendKind::Qemu,
            cpu: value
                .cpu
                .ok_or(ConvertError::MissingField("cpu"))?
                .try_into()?,
            memory: value
                .memory
                .ok_or(ConvertError::MissingField("memory"))?
                .into(),
            disk: value
                .disk
                .ok_or(ConvertError::MissingField("disk"))?
                .try_into()?,
            display: value
                .display
                .ok_or(ConvertError::MissingField("display"))?
                .try_into()?,
            gpu: value
                .gpu
                .ok_or(ConvertError::MissingField("gpu"))?
                .try_into()?,
            network: value
                .network
                .ok_or(ConvertError::MissingField("network"))?
                .try_into()?,
            firmware: value
                .firmware
                .ok_or(ConvertError::MissingField("firmware"))?
                .into(),
            audio: value
                .audio
                .ok_or(ConvertError::MissingField("audio"))?
                .try_into()?,
            input: value
                .input
                .ok_or(ConvertError::MissingField("input"))?
                .into(),
        })
    }
}

// --- GetInstanceConfig (domain -> proto только; сервер никогда не
// парсит GetInstanceConfigResponse обратно — это чисто исходящий ответ) -

impl From<BackendKind> for proto::BackendKind {
    fn from(value: BackendKind) -> Self {
        match value {
            BackendKind::Qemu => proto::BackendKind::Qemu,
            BackendKind::Vmm => proto::BackendKind::Vmm,
        }
    }
}

impl From<InstanceKind> for proto::InstanceKind {
    fn from(value: InstanceKind) -> Self {
        use proto::instance_kind::Kind;

        let kind = match value {
            InstanceKind::LinuxVm {
                iso_path,
                cdrom_bus,
            } => {
                let mut msg = proto::instance_kind::LinuxVm {
                    iso_path: iso_path.to_string_lossy().into_owned(),
                    ..Default::default()
                };
                msg.set_cdrom_bus(cdrom_bus.into());
                Kind::LinuxVm(msg)
            }
            InstanceKind::AndroidVm { android_profile } => {
                Kind::AndroidVm(proto::instance_kind::AndroidVm {
                    android_profile: Some(android_profile.into()),
                })
            }
        };
        proto::InstanceKind { kind: Some(kind) }
    }
}

/// Собирает `GetInstanceConfigResponse` из доменного `InstanceConfig`
/// целиком. `From`, не `TryFrom` — в отличие от направления
/// proto -> domain (`CreateInstanceRequest -> InstanceConfig`, которое
/// может встретить `UNSPECIFIED`/отсутствующий oneof от клиента),
/// доменный `InstanceConfig` по построению полон, конвертация в proto не
/// может провалиться.
impl From<InstanceConfig> for proto::GetInstanceConfigResponse {
    fn from(value: InstanceConfig) -> Self {
        proto::GetInstanceConfigResponse {
            instance_id: value.id.0.to_string(),
            name: value.name,
            kind: Some(value.kind.into()),
            backend: proto::BackendKind::from(value.backend) as i32,
            cpu: Some(value.cpu.into()),
            memory: Some(value.memory.into()),
            disk: Some(value.disk.into()),
            display: Some(value.display.into()),
            gpu: Some(value.gpu.into()),
            network: Some(value.network.into()),
            firmware: Some(value.firmware.into()),
            audio: Some(value.audio.into()),
            input: Some(value.input.into()),
        }
    }
}

/// Парсит `instance_id` из proto-запроса (строка с UUID) в `InstanceId`.
/// Отдельная функция, а не `impl TryFrom<String> for InstanceId` в
/// `andler-core` — этот формат (строка из gRPC-запроса) специфичен для
/// протокола, не часть домена.
pub fn parse_instance_id(raw: &str) -> Result<andler_core::InstanceId, ConvertError> {
    if raw.is_empty() {
        return Err(ConvertError::MissingInstanceId);
    }
    uuid::Uuid::parse_str(raw)
        .map(andler_core::InstanceId)
        .map_err(|source| ConvertError::InvalidInstanceId(raw.to_string(), source))
}

/// Заполняет `InstanceStatusResponse` из `InstanceState` — `detail`
/// (диагностика backend'а, не часть FSM) заполняется отдельно вызывающей
/// стороной (`andler-daemon::service`), так как `InstanceState` сам по себе
/// её не несёт (см. `andler_core::backend::BackendStatus`).
pub fn instance_state_to_proto(state: &InstanceState) -> (proto::InstanceStateKind, String) {
    match state {
        InstanceState::Created => (proto::InstanceStateKind::Created, String::new()),
        InstanceState::Starting => (proto::InstanceStateKind::Starting, String::new()),
        InstanceState::Running => (proto::InstanceStateKind::Running, String::new()),
        InstanceState::Paused => (proto::InstanceStateKind::Paused, String::new()),
        InstanceState::Stopping => (proto::InstanceStateKind::Stopping, String::new()),
        InstanceState::Stopped => (proto::InstanceStateKind::Stopped, String::new()),
        InstanceState::Error { message } => {
            (proto::InstanceStateKind::Error, message.clone())
        }
    }
}

/// `Status` (`tonic`) — чужой тип для `andler-rpc`, но `ConvertError` —
/// локальный, так что `impl ForeignTrait<LocalType> for ForeignType` здесь
/// разрешён orphan rule (в отличие от попытки сделать то же самое в
/// `andler-daemon`, где оба типа чужие). Это и есть причина, по которой
/// конвертации лежат в `andler-rpc`, а не в крейте, который их использует.
impl From<ConvertError> for tonic::Status {
    fn from(err: ConvertError) -> Self {
        tonic::Status::invalid_argument(err.to_string())
    }
}

impl From<LogStreamSource> for proto::LogStreamSource {
    fn from(value: LogStreamSource) -> Self {
        match value {
            LogStreamSource::Stdout => proto::LogStreamSource::Stdout,
            LogStreamSource::Stderr => proto::LogStreamSource::Stderr,
        }
    }
}

/// Только domain -> proto: `LogLineResponse` — выходное сообщение
/// `rpc StreamInstanceLogs`, клиент его не присылает обратно, поэтому
/// `TryFrom<proto::LogLineResponse> for LogLine` сейчас не нужен (в
/// отличие от большинства других типов в этом файле, у которых есть оба
/// направления, потому что соответствующие proto-типы — это запросы,
/// приходящие от клиента).
impl From<LogLine> for proto::LogLineResponse {
    fn from(value: LogLine) -> Self {
        let mut msg = proto::LogLineResponse {
            line: value.line,
            ..Default::default()
        };
        msg.set_source(value.source.into());
        msg
    }
}

impl From<ResourceMetrics> for proto::ResourceMetricsResponse {
    fn from(m: ResourceMetrics) -> Self {
        proto::ResourceMetricsResponse {
            cpu_percent: m.cpu_percent,
            memory_used_bytes: m.memory_used_bytes,
            disk_read_bytes_per_sec: m.disk_read_bytes_per_sec,
            disk_write_bytes_per_sec: m.disk_write_bytes_per_sec,
            net_rx_bytes_per_sec: m.net_rx_bytes_per_sec,
            net_tx_bytes_per_sec: m.net_tx_bytes_per_sec,
            vram_used_bytes: m.vram_used_bytes,
            vram_total_bytes: m.vram_total_bytes,
            gpu_load_percent: m.gpu_load_percent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_profile_round_trips_through_proto() {
        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            arm_translator: ArmTranslator::Libndk,
            root: RootMode::Magisk,
        };

        let msg: proto::AndroidProfile = profile.clone().into();
        let back: AndroidProfile = msg.try_into().unwrap();
        assert_eq!(profile, back);
    }

    #[test]
    fn android_profile_unspecified_arm_translator_falls_back_to_none() {
        // Старые сохранённые профили не могли заполнить это поле —
        // должны читаться как ArmTranslator::None, не как ошибка.
        let mut msg = proto::AndroidProfile {
            gapps: false,
            microg: false,
            root: proto::RootMode::None as i32,
            ..Default::default()
        };
        msg.set_android_version(proto::AndroidVersion::Android13);
        msg.set_arm_translator(proto::ArmTranslator::Unspecified);

        let profile = AndroidProfile::try_from(msg).unwrap();
        assert_eq!(profile.arm_translator, ArmTranslator::None);
    }

    #[test]
    fn unspecified_android_version_is_rejected() {
        let msg = proto::AndroidProfile {
            android_version: proto::AndroidVersion::Unspecified as i32,
            gapps: false,
            microg: false,
            root: proto::RootMode::None as i32,
            ..Default::default()
        };
        let err = AndroidProfile::try_from(msg).unwrap_err();
        assert!(matches!(err, ConvertError::MissingAndroidVersion));
    }

    #[test]
    fn parse_instance_id_rejects_empty_string() {
        let err = parse_instance_id("").unwrap_err();
        assert!(matches!(err, ConvertError::MissingInstanceId));
    }

    #[test]
    fn parse_instance_id_rejects_garbage() {
        let err = parse_instance_id("not-a-uuid").unwrap_err();
        assert!(matches!(err, ConvertError::InvalidInstanceId(_, _)));
    }

    #[test]
    fn parse_instance_id_accepts_valid_uuid() {
        let id = andler_core::InstanceId::new();
        let parsed = parse_instance_id(&id.0.to_string()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn error_state_carries_message_into_detail_tuple() {
        let state = InstanceState::Error {
            message: "boom".to_string(),
        };
        let (kind, message) = instance_state_to_proto(&state);
        assert_eq!(kind, proto::InstanceStateKind::Error);
        assert_eq!(message, "boom");
    }

    // --- InstanceConfig (CreateInstanceRequest, LinuxVm) -----------------

    fn sample_instance_config() -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::VirtioScsi,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    /// `InstanceConfig` сам по себе не конвертируется обратно в
    /// `proto::CreateInstanceRequest` (нет смысла — `id`/`backend` не
    /// часть запроса, см. комментарий у `TryFrom<CreateInstanceRequest>`),
    /// поэтому round-trip строится через отдельные конвертации каждого
    /// под-типа в обе стороны, а не через один сквозной `From`/`TryFrom`
    /// на уровне всего сообщения.
    fn instance_config_to_create_request(cfg: &InstanceConfig) -> proto::CreateInstanceRequest {
        let (iso_path, cdrom_bus) = match &cfg.kind {
            InstanceKind::LinuxVm {
                iso_path,
                cdrom_bus,
            } => (iso_path.to_string_lossy().into_owned(), *cdrom_bus),
            other => panic!("sample_instance_config produced non-LinuxVm kind: {other:?}"),
        };

        let mut request = proto::CreateInstanceRequest {
            name: cfg.name.clone(),
            iso_path,
            cpu: Some(cfg.cpu.clone().into()),
            memory: Some(cfg.memory.clone().into()),
            disk: Some(cfg.disk.clone().into()),
            display: Some(cfg.display.into()),
            gpu: Some(cfg.gpu.clone().into()),
            network: Some(cfg.network.clone().into()),
            firmware: Some(cfg.firmware.clone().into()),
            audio: Some(cfg.audio.into()),
            input: Some(cfg.input.into()),
            ..Default::default()
        };
        request.set_cdrom_bus(cdrom_bus.into());
        request
    }

    #[test]
    fn create_instance_request_round_trips_into_instance_config() {
        let original = sample_instance_config();
        let request = instance_config_to_create_request(&original);

        let converted = InstanceConfig::try_from(request).unwrap();

        // `id` не часть запроса (генерируется конвертацией), поэтому
        // сравниваем все поля, кроме `id`, а не весь struct целиком.
        assert_eq!(converted.name, original.name);
        assert_eq!(converted.kind, original.kind);
        assert_eq!(converted.backend, original.backend);
        assert_eq!(converted.cpu, original.cpu);
        assert_eq!(converted.memory, original.memory);
        assert_eq!(converted.disk, original.disk);
        assert_eq!(converted.display, original.display);
        assert_eq!(converted.gpu, original.gpu);
        assert_eq!(converted.network, original.network);
        assert_eq!(converted.firmware, original.firmware);
        assert_eq!(converted.audio, original.audio);
        assert_eq!(converted.input, original.input);
    }

    #[test]
    fn create_instance_request_with_passthrough_gpu_round_trips() {
        // Passthrough — единственный вариант RenderBackend с полем,
        // отдельный тест проверяет, что oneof-ветка с данными (а не
        // просто пустой message-маркер) переживает round-trip.
        let mut cfg = sample_instance_config();
        cfg.gpu.render_backend = RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        };
        let request = instance_config_to_create_request(&cfg);

        let converted = InstanceConfig::try_from(request).unwrap();
        assert_eq!(converted.gpu.render_backend, cfg.gpu.render_backend);
    }

    #[test]
    fn create_instance_request_with_bridge_network_round_trips() {
        let mut cfg = sample_instance_config();
        cfg.network.mode = NetworkMode::Bridge {
            interface: "br0".to_string(),
        };
        let request = instance_config_to_create_request(&cfg);

        let converted = InstanceConfig::try_from(request).unwrap();
        assert_eq!(converted.network.mode, cfg.network.mode);
    }

    #[test]
    fn create_instance_request_with_overlay_disk_round_trips_base_image() {
        let mut cfg = sample_instance_config();
        cfg.disk = DiskConfig::overlay(
            PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2"),
            PathBuf::from("/var/lib/andler/images/base.qcow2"),
            20 * DiskConfig::GIB,
        );
        let request = instance_config_to_create_request(&cfg);

        let converted = InstanceConfig::try_from(request).unwrap();
        assert_eq!(converted.disk, cfg.disk);
    }

    #[test]
    fn create_instance_request_missing_cpu_field_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.cpu = None;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(err, ConvertError::MissingField("cpu")));
    }

    #[test]
    fn create_instance_request_missing_display_resolution_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.display.as_mut().unwrap().resolution = None;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(
            err,
            ConvertError::MissingField("display.resolution")
        ));
    }

    #[test]
    fn create_instance_request_missing_render_backend_kind_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.gpu.as_mut().unwrap().render_backend.as_mut().unwrap().kind = None;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(err, ConvertError::MissingRenderBackendKind));
    }

    #[test]
    fn create_instance_request_missing_network_mode_kind_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.network.as_mut().unwrap().mode.as_mut().unwrap().kind = None;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(err, ConvertError::MissingNetworkModeKind));
    }

    #[test]
    fn create_instance_request_unspecified_disk_format_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.disk.as_mut().unwrap().format = proto::DiskFormat::Unspecified as i32;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(err, ConvertError::MissingDiskFormat));
    }

    #[test]
    fn create_instance_request_unspecified_cdrom_bus_defaults_to_ide() {
        // В отличие от disk_format, unspecified cdrom_bus — не ошибка
        // конфигурации, а сигнал "явного выбора не было" — см.
        // doc-комментарий `enum CdromBus` в `andler.proto`.
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.cdrom_bus = proto::CdromBus::Unspecified as i32;

        let converted = InstanceConfig::try_from(request).unwrap();
        match converted.kind {
            InstanceKind::LinuxVm { cdrom_bus, .. } => {
                assert_eq!(cdrom_bus, CdromBus::default());
                assert_eq!(cdrom_bus, CdromBus::Ide);
            }
            other => panic!("expected LinuxVm, got {other:?}"),
        }
    }

    #[test]
    fn create_instance_request_unspecified_audio_backend_is_rejected() {
        let cfg = sample_instance_config();
        let mut request = instance_config_to_create_request(&cfg);
        request.audio.as_mut().unwrap().backend = proto::AudioBackend::Unspecified as i32;

        let err = InstanceConfig::try_from(request).unwrap_err();
        assert!(matches!(err, ConvertError::MissingAudioBackend));
    }

    #[test]
    fn cpu_config_with_empty_affinity_round_trips_as_none() {
        let cfg = CpuConfig::reference_default();
        assert_eq!(cfg.affinity, None);

        let msg: proto::CpuConfig = cfg.clone().into();
        assert!(msg.affinity.is_empty());

        let back = CpuConfig::try_from(msg).unwrap();
        assert_eq!(back.affinity, None);
    }

    #[test]
    fn cpu_config_with_affinity_round_trips() {
        let mut cfg = CpuConfig::reference_default();
        cfg.affinity = Some(vec![0, 2, 4, 6]);

        let msg: proto::CpuConfig = cfg.clone().into();
        let back = CpuConfig::try_from(msg).unwrap();
        assert_eq!(back.affinity, cfg.affinity);
    }

    // --- GetInstanceConfigResponse ---------------------------------------

    #[test]
    fn get_instance_config_response_preserves_linux_vm_kind_and_id() {
        let cfg = sample_instance_config();
        let id = cfg.id;
        let response: proto::GetInstanceConfigResponse = cfg.into();

        assert_eq!(response.instance_id, id.0.to_string());
        assert_eq!(response.name, "test-vm");
        assert_eq!(response.backend(), proto::BackendKind::Qemu);

        use proto::instance_kind::Kind;
        match response.kind.expect("kind must be Some").kind {
            Some(Kind::LinuxVm(linux_vm)) => {
                assert_eq!(linux_vm.iso_path, "/tmp/test.iso");
                assert_eq!(linux_vm.cdrom_bus(), proto::CdromBus::VirtioScsi);
            }
            other => panic!("expected LinuxVm kind, got {other:?}"),
        }
    }

    #[test]
    fn get_instance_config_response_preserves_android_vm_kind() {
        let mut cfg = sample_instance_config();
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: true,
                microg: false,
                arm_translator: ArmTranslator::None,
                root: RootMode::Magisk,
            },
        };
        let response: proto::GetInstanceConfigResponse = cfg.into();

        use proto::instance_kind::Kind;
        match response.kind.expect("kind must be Some").kind {
            Some(Kind::AndroidVm(android_vm)) => {
                let profile = android_vm
                    .android_profile
                    .expect("android_profile must be Some");
                assert!(profile.gapps);
                assert_eq!(profile.root(), proto::RootMode::Magisk);
            }
            other => panic!("expected AndroidVm kind, got {other:?}"),
        }
    }

    #[test]
    fn get_instance_config_response_carries_every_sub_config() {
        // Не точечная проверка одного поля — все 9 секций должны
        // присутствовать как Some, иначе andler-cli получил бы Option
        // None там, где ожидает заполненную секцию для печати.
        let cfg = sample_instance_config();
        let response: proto::GetInstanceConfigResponse = cfg.into();

        assert!(response.cpu.is_some());
        assert!(response.memory.is_some());
        assert!(response.disk.is_some());
        assert!(response.display.is_some());
        assert!(response.gpu.is_some());
        assert!(response.network.is_some());
        assert!(response.firmware.is_some());
        assert!(response.audio.is_some());
        assert!(response.input.is_some());
    }

    #[test]
    fn display_engine_none_round_trips_through_proto() {
        // DISPLAY_NONE не делит префикс с DisplayEngineUnspecified (в
        // отличие от Sdl/Spice/Dbus, которые тоже не делят, но это уже
        // было покрыто реальным успешным прогоном) — добавлен этот тест
        // в первую очередь чтобы зафиксировать точное сгенерированное
        // имя `prost` (`DisplayNone`) как контракт, не только проверить
        // логику round-trip.
        let msg = proto::DisplayEngine::DisplayNone;
        let domain = DisplayEngine::try_from(msg).unwrap();
        assert_eq!(domain, DisplayEngine::None);

        let back: proto::DisplayEngine = domain.into();
        assert_eq!(back, proto::DisplayEngine::DisplayNone);
    }

    #[test]
    fn display_engine_gtk_round_trips_through_proto() {
        let msg = proto::DisplayEngine::Gtk;
        let domain = DisplayEngine::try_from(msg).unwrap();
        assert_eq!(domain, DisplayEngine::Gtk);

        let back: proto::DisplayEngine = domain.into();
        assert_eq!(back, proto::DisplayEngine::Gtk);
    }

    #[test]
    fn nat_backend_unspecified_falls_back_to_slirp() {
        // Старые сохранённые конфиги не могли заполнить это поле —
        // должны читаться как Slirp (поведение start.sh), не как ошибка.
        let domain: NatBackend = proto::NatBackend::Unspecified.into();
        assert_eq!(domain, NatBackend::Slirp);
    }

    #[test]
    fn nat_backend_passt_round_trips_through_proto() {
        let domain: NatBackend = proto::NatBackend::Passt.into();
        assert_eq!(domain, NatBackend::Passt);
        let back: proto::NatBackend = domain.into();
        assert_eq!(back, proto::NatBackend::Passt);
    }

    #[test]
    fn audio_device_unspecified_falls_back_to_virtio_sound() {
        let domain: AudioDevice = proto::AudioDevice::Unspecified.into();
        assert_eq!(domain, AudioDevice::VirtioSound);
    }

    #[test]
    fn audio_device_ich9_hda_round_trips_through_proto() {
        let domain: AudioDevice = proto::AudioDevice::Ich9Hda.into();
        assert_eq!(domain, AudioDevice::Ich9Hda);
        let back: proto::AudioDevice = domain.into();
        assert_eq!(back, proto::AudioDevice::Ich9Hda);
    }

    #[test]
    fn input_config_pointer_mode_unspecified_falls_back_to_legacy_tablet_mode() {
        // Старый клиент прислал только `tablet_mode = true`, ничего не
        // зная про `pointer_mode` — должны получить Tablet, не Mouse.
        let msg = proto::InputConfig {
            tablet_mode: true,
            hide_host_cursor: true,
            clipboard_enabled: true,
            pointer_mode: proto::PointerMode::Unspecified as i32,
        };
        let domain: InputConfig = msg.into();
        assert_eq!(domain.pointer_mode, PointerMode::Tablet);
    }

    #[test]
    fn input_config_pointer_mode_explicit_wins_over_legacy_tablet_mode() {
        // pointer_mode=Mouse побеждает, даже если устаревшее поле
        // tablet_mode=true (рассинхронизированный/старый клиент).
        let msg = proto::InputConfig {
            tablet_mode: true,
            hide_host_cursor: true,
            clipboard_enabled: true,
            pointer_mode: proto::PointerMode::Mouse as i32,
        };
        let domain: InputConfig = msg.into();
        assert_eq!(domain.pointer_mode, PointerMode::Mouse);
    }

    #[test]
    fn input_config_to_proto_fills_legacy_tablet_mode_field() {
        // Новый код тоже заполняет устаревшее bool-поле — старые клиенты,
        // которые ещё не знают pointer_mode, продолжают работать.
        let domain = InputConfig {
            pointer_mode: PointerMode::Tablet,
            hide_host_cursor: true,
            clipboard_enabled: true,
        };
        let msg: proto::InputConfig = domain.into();
        assert!(msg.tablet_mode);
        assert_eq!(msg.pointer_mode(), proto::PointerMode::Tablet);
    }

    #[test]
    fn log_line_converts_to_proto_preserving_source_and_text() {
        let stdout_line = LogLine {
            source: LogStreamSource::Stdout,
            line: "VNC server running".to_string(),
        };
        let msg: proto::LogLineResponse = stdout_line.into();
        assert_eq!(msg.source(), proto::LogStreamSource::Stdout);
        assert_eq!(msg.line, "VNC server running");

        let stderr_line = LogLine {
            source: LogStreamSource::Stderr,
            line: "qemu-system-x86_64: warning: ...".to_string(),
        };
        let msg: proto::LogLineResponse = stderr_line.into();
        assert_eq!(msg.source(), proto::LogStreamSource::Stderr);
    }

    #[test]
    fn clone_mode_round_trips_through_proto_for_every_variant() {
        for (domain, proto_variant) in [
            (CloneMode::Linked, proto::CloneMode::Linked),
            (CloneMode::FullStandalone, proto::CloneMode::FullStandalone),
            (CloneMode::SharedBase, proto::CloneMode::SharedBase),
        ] {
            let converted = CloneMode::try_from(proto_variant).unwrap();
            assert_eq!(converted, domain);
        }
    }

    #[test]
    fn unspecified_clone_mode_is_rejected() {
        let err = CloneMode::try_from(proto::CloneMode::Unspecified).unwrap_err();
        assert!(matches!(err, ConvertError::MissingCloneMode));
    }

    #[test]
    fn resource_metrics_converts_to_proto_preserving_all_fields() {
        let metrics = ResourceMetrics {
            cpu_percent: Some(42.5),
            memory_used_bytes: Some(1024 * 1024 * 512),
            disk_read_bytes_per_sec: Some(1024 * 100),
            disk_write_bytes_per_sec: Some(1024 * 50),
            net_rx_bytes_per_sec: Some(1024 * 200),
            net_tx_bytes_per_sec: Some(1024 * 150),
            vram_used_bytes: Some(1024 * 1024 * 256),
            vram_total_bytes: Some(1024 * 1024 * 1024 * 8),
            gpu_load_percent: Some(73.0),
        };
        let msg: proto::ResourceMetricsResponse = metrics.into();
        assert!((msg.cpu_percent.unwrap() - 42.5).abs() < f32::EPSILON);
        assert_eq!(msg.memory_used_bytes, Some(1024 * 1024 * 512));
        assert_eq!(msg.disk_read_bytes_per_sec, Some(1024 * 100));
        assert_eq!(msg.disk_write_bytes_per_sec, Some(1024 * 50));
        assert_eq!(msg.net_rx_bytes_per_sec, Some(1024 * 200));
        assert_eq!(msg.net_tx_bytes_per_sec, Some(1024 * 150));
        assert_eq!(msg.vram_used_bytes, Some(1024 * 1024 * 256));
        assert_eq!(msg.vram_total_bytes, Some(1024 * 1024 * 1024 * 8));
        assert!((msg.gpu_load_percent.unwrap() - 73.0).abs() < f32::EPSILON);
    }

    #[test]
    fn resource_metrics_with_none_fields_converts_to_empty_proto() {
        let metrics = ResourceMetrics::default();
        let msg: proto::ResourceMetricsResponse = metrics.into();
        assert!(msg.cpu_percent.is_none());
        assert!(msg.memory_used_bytes.is_none());
        assert!(msg.disk_read_bytes_per_sec.is_none());
        assert!(msg.disk_write_bytes_per_sec.is_none());
        assert!(msg.net_rx_bytes_per_sec.is_none());
        assert!(msg.net_tx_bytes_per_sec.is_none());
        assert!(msg.vram_used_bytes.is_none());
        assert!(msg.vram_total_bytes.is_none());
        assert!(msg.gpu_load_percent.is_none());
    }
}
