//! Выбор bus для CD-ROM/ISO-привода.
//!
//! См. PLAN.md, раздел «Монтирование ISO / CD-ROM»: диск (`virtio-blk-pci`)
//! и CD-ROM-привод — разные устройства с разными правилами выбора.
//! `virtio-blk-pci` не способен монтировать CD-ROM вообще, поэтому выбор
//! bus для ISO — это отдельная ось конфигурации, не "тот же virtio, что у
//! диска".
//!
//! Риск асимметричен между диском и приводом: если выбор окажется
//! неподходящим для основного диска — это маловероятно (почти все
//! современные ОС несут `virtio-blk` inbox), и в любом случае ошибка
//! проявится уже внутри загруженной системы, которую можно исправить. Но
//! если `virtio-scsi`-привод для ISO не сработает, инсталлятор не сможет
//! прочитать установочные файлы на этапе самого раннего boot — то есть
//! пользователь застрянет до того, как у него появится возможность
//! что-либо поменять.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Bus, через который ANDLER подключает ISO/CD-ROM привод к гостю.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CdromBus {
    /// `virtio-scsi-pci` + `scsi-cd`. Быстрее монтирование и чтение
    /// установочных файлов. Требует, чтобы initrd/загрузчик на самом ISO
    /// уже включал virtio-scsi модуль на этапе раннего boot — почти
    /// всегда верно для современных Linux-дистрибутивов (CachyOS,
    /// Ubuntu, Fedora, Arch и т. п.) и для Android base-image'ей,
    /// собираемых самим ANDLER. Если установка не загружается —
    /// переключитесь на `Ide`.
    VirtioScsi,
    /// `ide-cd`. Медленнее монтирование/чтение, но работает без каких-либо
    /// virtio-драйверов в загрузочной среде — IDE поддерживается на
    /// уровне firmware/BIOS практически любой ОС. Безопасный выбор для
    /// неизвестного/произвольного ISO, для Windows и для любого гостя, у
    /// которого нет подтверждённой virtio-scsi поддержки на этапе
    /// раннего boot (см. также практику Red Hat/OKD: `virtio-win.iso`
    /// монтируется именно как IDE/SATA CD-ROM, не через virtio-scsi —
    /// курица-и-яйцо, virtio-scsi драйвер не может быть прочитан с
    /// CD-привода, который сам требует virtio-scsi для чтения).
    Ide,
}

impl CdromBus {
    /// Короткое объяснение разницы и условия выбора варианта — единый
    /// источник текста для CLI `--help`, wizard-подсказок
    /// (`inquire::Select::with_help_message`) и будущих GUI-тултипов. См.
    /// PLAN.md, раздел «Принцип для GUI/CLI: явный выбор + объяснение
    /// разницы»: doc-комментарий на варианте — то же самое содержание,
    /// просто продублированное в рантайм-доступном виде, потому что
    /// doc-комментарии сами по себе недоступны через `std::any`/reflection
    /// в Rust без отдельного proc-macro. Если набор мест, использующих
    /// этот текст, вырастет — стоит подключить derive-макрос, который
    /// генерирует эту функцию из самих doc-комментариев, а не
    /// поддерживать два текста вручную в синхронизированном виде.
    pub fn description(&self) -> &'static str {
        match self {
            CdromBus::VirtioScsi => {
                "Быстрее монтирование и чтение установочных файлов. Подходит, если \
                 установочный носитель — современный Linux-дистрибутив (initrd почти \
                 всегда поддерживает virtio-scsi). Если установка не загружается — \
                 переключитесь на ide."
            }
            CdromBus::Ide => {
                "Медленнее, но работает без virtio-драйверов в загрузочной среде — \
                 совместимо с любым гостем (Windows, неизвестный ISO, старые ОС)."
            }
        }
    }

    /// Имена известных Linux-дистрибутивов, для которых ANDLER считает
    /// virtio-scsi безопасным дефолтом — см. PLAN.md, раздел «Правило
    /// выбора дефолта»: «Известный Linux-дистрибутив (распознан wizard'ом
    /// по имени файла ISO...) → virtio-scsi-pci + scsi-cd дефолтом».
    /// Список сознательно ограничен дистрибутивами с современными LiveISO
    /// (initrd с virtio-scsi почти гарантированно), не пытается покрыть
    /// все существующие дистрибутивы — для остальных используется
    /// безопасный fallback `Ide`, который пользователь может явно
    /// переопределить.
    const KNOWN_VIRTIO_FRIENDLY_DISTROS: &'static [&'static str] = &[
        "cachyos",
        "ubuntu",
        "kubuntu",
        "xubuntu",
        "lubuntu",
        "fedora",
        "debian",
        "arch",
        "manjaro",
        "mint",
        "pop-os",
        "pop_os",
        "opensuse",
        "endeavouros",
        "garuda",
        "kali",
        "nixos",
    ];

    /// Условный дефолт по имени файла ISO — см. PLAN.md, раздел
    /// «Монтирование ISO / CD-ROM» → «Правило выбора дефолта». Это
    /// эвристика для CLI/wizard, не часть домена: при отсутствии явного
    /// выбора пользователя CLI/wizard должны вызывать эту функцию и
    /// предлагать результат как дефолт, который пользователь всегда может
    /// переопределить (`--cdrom-bus virtio|ide`).
    ///
    /// Имя файла приводится к нижнему регистру и ищется по подстроке —
    /// этого достаточно для типичных имён вида
    /// `cachyos-desktop-linux-260426.iso`, `ubuntu-24.04-desktop-amd64.iso`.
    /// Если совпадений нет (неизвестный ISO, Windows-образ и т. п.) —
    /// безопасный fallback `Ide`, см. doc-комментарий модуля про
    /// асимметрию риска.
    pub fn recommended_for_iso_filename(iso_path: &Path) -> Self {
        let Some(file_name) = iso_path.file_name().and_then(|n| n.to_str()) else {
            return CdromBus::Ide;
        };
        let lower = file_name.to_lowercase();

        if Self::KNOWN_VIRTIO_FRIENDLY_DISTROS
            .iter()
            .any(|distro| lower.contains(distro))
        {
            CdromBus::VirtioScsi
        } else {
            CdromBus::Ide
        }
    }
}

impl Default for CdromBus {
    /// `Ide` — безопасный fallback, не статичный "лучший по умолчанию"
    /// выбор. Используется там, где нет иной информации для принятия
    /// условного решения (например, при десериализации proto-сообщения
    /// со значением `CDROM_BUS_UNSPECIFIED`). Предпочтительный путь —
    /// `recommended_for_iso_filename`, не этот `Default`.
    fn default() -> Self {
        CdromBus::Ide
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recommends_virtio_scsi_for_known_distros() {
        for name in [
            "cachyos-desktop-linux-260426.iso",
            "ubuntu-24.04-desktop-amd64.iso",
            "Fedora-Workstation-Live-x86_64-41.iso",
            "archlinux-2026.06.01-x86_64.iso",
            "manjaro-kde-26.0-minimal-linux616.iso",
        ] {
            assert_eq!(
                CdromBus::recommended_for_iso_filename(&PathBuf::from(name)),
                CdromBus::VirtioScsi,
                "expected VirtioScsi for {name}"
            );
        }
    }

    #[test]
    fn recommends_ide_for_unknown_or_windows_iso() {
        for name in [
            "Win11_24H2_English_x64.iso",
            "random-image.iso",
            "custom-build.iso",
        ] {
            assert_eq!(
                CdromBus::recommended_for_iso_filename(&PathBuf::from(name)),
                CdromBus::Ide,
                "expected Ide for {name}"
            );
        }
    }

    #[test]
    fn recommends_ide_when_path_has_no_filename() {
        assert_eq!(
            CdromBus::recommended_for_iso_filename(&PathBuf::from("/")),
            CdromBus::Ide
        );
    }

    #[test]
    fn default_is_ide_not_virtio() {
        // Безопасный fallback, не "лучший по умолчанию" — см. doc-комментарий.
        assert_eq!(CdromBus::default(), CdromBus::Ide);
    }
}
