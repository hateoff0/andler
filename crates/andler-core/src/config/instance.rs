//! Идентификатор инстанса, его тип и итоговая `InstanceConfig`,
//! объединяющая все остальные конфигурационные типы из `config/`.
//!
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §4.1, §4.3, §4.4.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    AudioConfig, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig, InputConfig,
    MemoryConfig, NetworkConfig,
};
use crate::android_profile::AndroidProfile;

/// Уникальный идентификатор инстанса. Генерируется при создании
/// (`andler-daemon`) и используется как первичный ключ в `andler-store`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub Uuid);

impl InstanceId {
    pub fn new() -> Self {
        InstanceId(Uuid::new_v4())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

/// Какой backend гипервизора используется для инстанса. Определяет, какая
/// реализация `HypervisorBackend` (см. `backend.rs`) будет вызвана
/// `andler-daemon` из реестра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    Qemu,
    /// См. README `andler-vmm` — зарегистрирован, но методы возвращают
    /// `BackendError::NotImplemented`.
    Vmm,
}

/// Тип гостевой системы инстанса.
///
/// `AndroidVm` — это не отдельный движок виртуализации, а `LinuxVm`-подобный
/// запуск с заранее собранным гостевым образом (Waydroid внутри) и
/// overlay-диском поверх него. На уровне `andler-qemu::cmdline` разницы
/// между вариантами нет — она целиком в том, какой `DiskConfig` подставлен
/// в итоговый `InstanceConfig`. См. §4.4 архитектурного плана.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceKind {
    /// Обычная Linux VM, загружаемая с указанного ISO (первый запуск)
    /// либо с уже установленного диска (`iso_path` тогда не используется
    /// при сборке cmdline, но сохраняется для возможности переустановки —
    /// конкретное поведение решает `andler-qemu`).
    LinuxVm { iso_path: PathBuf },
    /// Android-инстанс на базе Waydroid. `android_profile` резолвится в
    /// конкретный `DiskConfig` через `AndroidProfile::resolve`
    /// (см. `android_profile.rs`) до того, как `InstanceConfig` будет
    /// передан в `HypervisorBackend::spawn`.
    AndroidVm { android_profile: AndroidProfile },
}

/// Полная конфигурация инстанса — то, что `HypervisorBackend::spawn`
/// получает на вход и что персистентно хранится в `andler-store`.
///
/// Это чистые данные без логики: сборка валидного `InstanceConfig` для
/// `AndroidVm` (резолв образа, создание overlay-диска) происходит до
/// конструирования этого типа, в `andler-daemon`/`andler-core::android_profile`
/// — сам тип ничего не скачивает и не создаёт файлов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub id: InstanceId,
    pub name: String,
    pub kind: InstanceKind,
    pub backend: BackendKind,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,
    pub disk: DiskConfig,
    pub display: DisplayConfig,
    pub gpu: GpuConfig,
    pub network: NetworkConfig,
    /// UEFI/OVMF firmware. См. `firmware.rs` — добавлено отдельно от
    /// исходного §4.3 архитектурного плана при реализации
    /// `andler-qemu::cmdline`, так как `start.sh` требует pflash-дисков,
    /// для которых план не выделял тип.
    pub firmware: FirmwareConfig,
    pub audio: AudioConfig,
    pub input: InputConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_unique() {
        let a = InstanceId::new();
        let b = InstanceId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn config_round_trips_through_serde_json() {
        let cfg = InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/cachyos.iso"),
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("test-vm_VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        };

        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: InstanceConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, restored);
    }
}
