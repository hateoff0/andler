use std::path::PathBuf;

use super::Daemon;
use super::error::DaemonError;
use andler_core::{
    CloneMode, InstanceConfig, InstanceId, InstanceKind, InstanceState,
};

impl Daemon {
    /// Клонирует существующий инстанс (`LinuxVm` или `AndroidVm`) в
    /// новый, независимый `InstanceId` — три режима диска (`CloneMode`,
    /// см. его документацию за полным обоснованием), общая для всех трёх
    /// логика вокруг этого: проверка состояния источника, копирование
    /// `OVMF_VARS`, сборка нового `InstanceConfig`, регистрация через
    /// `create_instance`.
    ///
    /// `LinuxVm` поддерживается для режимов `Linked`/`FullStandalone` —
    /// disk-операции типо-agnostic и работают с любым qcow2. Режим
    /// `SharedBase` требует общий `base_image` профиля (есть только у
    /// `AndroidVm`), попытка использовать его для `LinuxVm` возвращает
    /// `DaemonError::SharedBaseNotSupportedForLinuxVm`.
    ///
    /// Источник должен быть в терминальном состоянии
    /// (`Created`/`Stopped`/`Error`) — копировать/линковать диск живого
    /// процесса QEMU небезопасно (см. `DaemonError::InstanceNotClonable`).
    ///
    /// `instances_root` — тот же смысл, что и у `create_android_instance`
    /// (не хардкодится, чтобы тесты могли передать временный каталог).
    ///
    /// При сбое посередине (после создания каталога клона, но до
    /// успешной регистрации) — `InstanceDirGuard` убирает частично
    /// созданный `instance_dir`.
    pub async fn clone_instance(
        &self,
        source_id: InstanceId,
        new_name: String,
        instances_root: PathBuf,
        mode: CloneMode,
    ) -> Result<InstanceId, DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;

        // Fail fast: `SharedBase` требует общий `base_image` — для
        // Android-инстанса он есть всегда (см. `DiskConfig::overlay`,
        // используемую `create_android_instance`), но тип
        // `DiskConfig.base_image: Option<PathBuf>` этого не
        // гарантирует статически. У `LinuxVm` нет `base_image`
        // вообще — диск standalone, `SharedBase` к нему неприменим.
        // Проверяем до любых файловых операций.
        if mode == CloneMode::SharedBase
            && !matches!(source_config.kind, InstanceKind::AndroidVm { .. })
        {
            return Err(DaemonError::SharedBaseNotSupportedForLinuxVm(source_id));
        }

        let new_id = InstanceId::new();
        let instance_dir = instances_root.join(new_id.0.to_string());
        let mut dir_guard = super::types::InstanceDirGuard::new(instance_dir.clone());

        tokio::fs::create_dir_all(&instance_dir)
            .await
            .map_err(|source| DaemonError::Io {
                path: instance_dir.clone(),
                source,
            })?;

        let new_ovmf_vars_path = instance_dir.join("VARS.fd");
        tokio::fs::copy(&source_config.firmware.ovmf_vars_path, &new_ovmf_vars_path)
            .await
            .map_err(|source| DaemonError::Io {
                path: new_ovmf_vars_path.clone(),
                source,
            })?;

        let new_disk_path = instance_dir.join("disk.qcow2");
        let cloned_disk = match mode {
            CloneMode::Linked => {
                andler_disk::clone::linked_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    source_config.disk.size_bytes,
                )
                .await?
            }
            CloneMode::FullStandalone => {
                andler_disk::clone::full_standalone_clone(&source_config.disk.path, &new_disk_path)
                    .await?
            }
            CloneMode::SharedBase => {
                let base_image = source_config.disk.base_image.clone().ok_or_else(|| {
                    DaemonError::Io {
                        path: source_config.disk.path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "SharedBase clone requires a source disk with base_image set",
                        ),
                    }
                })?;
                andler_disk::clone::shared_base_clone(
                    &source_config.disk.path,
                    &new_disk_path,
                    &base_image,
                )
                .await?
            }
        };

        let mut new_config = source_config;
        new_config.id = new_id;
        new_config.name = new_name;
        new_config.disk.path = cloned_disk.disk_path;
        new_config.disk.base_image = cloned_disk.backing_file;
        new_config.firmware.ovmf_vars_path = new_ovmf_vars_path;

        let registered_id = self.create_instance(new_config).await?;
        dir_guard.disarm();

        Ok(registered_id)
    }

    /// Экспортирует диск инстанса (`LinuxVm` или `AndroidVm`) как
    /// самостоятельный файл по указанному пути — для переноса между
    /// хостами или бэкапа, не для создания нового управляемого инстанса.
    ///
    /// Реализовано через `andler_disk::clone::full_standalone_clone`
    /// (разворачивает всю backing chain, включая общий `base_image`, в
    /// один файл).
    ///
    /// Источник должен быть в терминальном состоянии — та же причина, что
    /// у `clone_instance`.
    pub async fn export_instance_disk(
        &self,
        source_id: InstanceId,
        dest_path: PathBuf,
    ) -> Result<(), DaemonError> {
        let source_config = self.terminal_clonable_instance_config(source_id).await?;

        andler_disk::clone::full_standalone_clone(&source_config.disk.path, &dest_path).await?;

        Ok(())
    }

    /// Общая проверка для `clone_instance`/`export_instance_disk`:
    /// инстанс существует и в терминальном состоянии (`Created`/`Stopped`/
    /// `Error`). Возвращает клон `InstanceConfig` источника (не ссылку —
    /// read-lock `instances` отпускается до того, как вызывающая сторона
    /// начинает файловые операции).
    async fn terminal_clonable_instance_config(
        &self,
        id: InstanceId,
    ) -> Result<InstanceConfig, DaemonError> {
        let instances = self.instances.read().await;
        let record = instances.get(&id).ok_or(DaemonError::InstanceNotFound(id))?;

        let clonable = matches!(
            record.state,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        );
        if !clonable {
            return Err(DaemonError::InstanceNotClonable(id, record.state.clone()));
        }

        Ok(record.config.clone())
    }

    /// Находит `InstanceId` всех инстансов, чей `disk.base_image`
    /// указывает прямо на диск инстанса `id` — то есть живых
    /// `CloneMode::Linked`-клонов этого инстанса.
    ///
    /// Сравнение по `disk.path` инстанса `id`, не по самому `id` —
    /// `base_image` в `InstanceConfig` хранит путь к файлу, не
    /// `InstanceId` (см. `DiskConfig`).
    pub async fn find_live_clones(&self, id: InstanceId) -> Result<Vec<InstanceId>, DaemonError> {
        let instances = self.instances.read().await;
        let target_disk_path = instances
            .get(&id)
            .map(|record| record.config.disk.path.clone())
            .ok_or(DaemonError::InstanceNotFound(id))?;

        let clones = instances
            .iter()
            .filter(|(other_id, record)| {
                **other_id != id && record.config.disk.base_image.as_deref() == Some(target_disk_path.as_path())
            })
            .map(|(other_id, _)| *other_id)
            .collect();

        Ok(clones)
    }
}
