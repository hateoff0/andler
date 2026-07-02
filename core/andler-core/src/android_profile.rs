//! `AndroidProfile` — параметры конкретного варианта Android-инстанса и
//! резолв в полноценный `InstanceConfig`.
//!
//! См. docs/architecture/CORE_ARCHITECTURE_PLAN.md, §4.4 — это
//! Rust-реализация того, что там описано на словах: какой базовый образ
//! нужен, как профиль превращается в overlay-диск и итоговую конфигурацию.
//!
//! ВАЖНО: `resolve()` в этом файле — чистая функция, она НЕ скачивает
//! образ и не создаёт overlay-файл на диске. Она принимает уже готовый
//! путь к базовому образу (`base_image_path`) — получение этого пути
//! (проверка кэша, скачивание, проверка подписи, см. §4.4.2 плана и
//! docs/architecture/GUEST_IMAGE_PLAN.md) — задача `andler-daemon` до
//! вызова `resolve()`, а создание самого overlay-файла на диске — задача
//! `andler-disk::overlay`, вызываемая `andler-daemon` после `resolve()`
//! и до `HypervisorBackend::spawn`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{
    AudioConfig, BackendKind, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig,
    InputConfig, InstanceConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
};

/// Версия Android внутри гостевого образа. Конкретный набор поддерживаемых
/// версий определяется тем, что реально собирается в `guest-image/`
/// (см. GUEST_IMAGE_PLAN.md) — список здесь может расширяться без
/// изменения остальной части `AndroidProfile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AndroidVersion {
    Android11,
    Android13,
}

/// Транслятор архитектур ARM -> x86, нужен для приложений, скомпилированных
/// только под ARM. Только один активен одновременно — это не набор флагов,
/// а выбор одного из трёх взаимоисключающих вариантов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ArmTranslator {
    /// Без транслятора — ARM-only приложения не запустятся.
    #[default]
    None,
    /// Рекомендуется для AMD CPU (см. `andler-firmware::detect::arm`).
    Libndk,
    /// Рекомендуется для Intel CPU (см. `andler-firmware::detect::arm`).
    Libhoudini,
}

/// Режим root-доступа внутри гостя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RootMode {
    #[default]
    None,
    Magisk,
}

/// Параметры конкретного варианта Android-инстанса — то, что пользователь
/// выбирает при создании `InstanceKind::AndroidVm`. Однозначно определяет,
/// какой вариант базового образа из матрицы сборки `guest-image/` нужен
/// (см. §3 GUEST_IMAGE_PLAN.md: версия × gapps/microg × наличие
/// транслятора — это и есть `cache_key()` ниже).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndroidProfile {
    pub android_version: AndroidVersion,
    pub gapps: bool,
    pub microg: bool,
    /// Транслятор архитектур ARM -> x86 (libhoudini/libndk) — нужен,
    /// если приложение скомпилировано только под ARM. См.
    /// [`ArmTranslator`].
    pub arm_translator: ArmTranslator,
    pub root: RootMode,
}

impl AndroidProfile {
    /// Ключ кэша/версионирования базового образа — однозначно определяет,
    /// какой именно артефакт из матрицы сборки `guest-image/` нужен этому
    /// профилю. `RootMode` сюда не входит: root применяется provisioning-
    /// шагом над уже готовым overlay (см. `resolve()` ниже), а не запекается
    /// в базовый образ, поэтому два профиля с разным `root`, но одинаковым
    /// остальным набором полей делят один и тот же базовый образ.
    pub fn cache_key(&self) -> String {
        format!(
            "{:?}-gapps_{}-microg_{}-arm_{:?}",
            self.android_version, self.gapps, self.microg, self.arm_translator
        )
    }

    /// Резолвит профиль в полноценный `InstanceConfig` с overlay-диском.
    ///
    /// `base_image_path` — путь к уже скачанному и проверенному базовому
    /// образу для этого профиля (см. модуль-документацию выше: ответственность
    /// за получение этого пути лежит на вызывающей стороне, не здесь).
    /// `overlay_path` — куда `andler-disk::overlay` создаст (или уже создал)
    /// overlay-диск конкретного инстанса. `ovmf_vars_path` — аналогично,
    /// персональная копия OVMF_VARS для этого инстанса (см. `FirmwareConfig`);
    /// её создание (копирование шаблона) — тоже задача вызывающей стороны,
    /// не этой функции.
    ///
    /// Render backend и параметры дисплея/CPU/памяти/audio/input берутся
    /// как разумные дефолты для Android-инстанса (см. `reference_default`
    /// у каждого конфига) — пресет (`PRESETS_PLAN.md`) применяется отдельным
    /// шагом `andler-daemon` уже над результатом этой функции, как частичный
    /// оверрайд, а не как замена этого резолва. См. §4.4.3 архитектурного
    /// плана.
    pub fn resolve(
        &self,
        instance_name: String,
        base_image_path: PathBuf,
        overlay_path: PathBuf,
        overlay_size_bytes: u64,
        ovmf_vars_path: PathBuf,
    ) -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: instance_name,
            kind: InstanceKind::AndroidVm {
                android_profile: self.clone(),
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::overlay(overlay_path, base_image_path, overlay_size_bytes),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(ovmf_vars_path),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> AndroidProfile {
        AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            arm_translator: ArmTranslator::Libndk,
            root: RootMode::None,
        }
    }

    #[test]
    fn cache_key_differs_on_arm_translator() {
        let mut a = sample_profile();
        let mut b = sample_profile();
        a.arm_translator = ArmTranslator::Libndk;
        b.arm_translator = ArmTranslator::Libhoudini;
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn cache_key_does_not_depend_on_root_mode() {
        let mut a = sample_profile();
        let mut b = sample_profile();
        a.root = RootMode::None;
        b.root = RootMode::Magisk;
        assert_eq!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn cache_key_differs_on_gapps() {
        let mut a = sample_profile();
        let mut b = sample_profile();
        a.gapps = true;
        b.gapps = false;
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn resolve_produces_overlay_disk_pointing_at_base_image() {
        let profile = sample_profile();
        let base = PathBuf::from("/var/lib/andler/images/android13-gapps-libndk.qcow2");
        let overlay = PathBuf::from("/var/lib/andler/instances/abc/disk.qcow2");

        let cfg = profile.resolve(
            "my-android".to_string(),
            base.clone(),
            overlay.clone(),
            20 * DiskConfig::GIB,
            PathBuf::from("/var/lib/andler/instances/abc/VARS.fd"),
        );

        assert_eq!(cfg.name, "my-android");
        assert_eq!(cfg.disk.base_image, Some(base));
        assert_eq!(cfg.disk.path, overlay);
        match cfg.kind {
            InstanceKind::AndroidVm { android_profile } => {
                assert_eq!(android_profile, profile);
            }
            InstanceKind::LinuxVm { .. } => panic!("expected AndroidVm"),
        }
    }
}
