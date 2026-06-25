//! Файл конфигурации `LinuxVm`-инстанса для `andler create` (TOML).
//!
//! Не `InstanceConfig` напрямую через `serde` — `InstanceConfig` несёт
//! `id`/`backend`, которые пользователь не должен (и не может) указать
//! сам (см. комментарий у `rpc CreateInstance` в `andler.proto`:
//! `id` генерируется демоном, `backend` всегда `BackendKind::Qemu`).
//! `InstanceFile` — зеркало именно `CreateInstanceRequest`, не более общего
//! доменного типа, по той же причине, по которой proto ограничен на
//! `LinuxVm`, а не `oneof kind`.
//!
//! Каждая секция (`cpu`/`memory`/`display`/`gpu`/`network`/`audio`/`input`)
//! — `Option`, отсутствие ⇒ `andler_core::config::*::reference_default()`.
//! `disk`/`firmware` исключение: оба требуют путь файла, для которого
//! дефолта не существует (нет разумного "дефолтного" места для диска
//! инстанса) — поэтому `disk_path`/`ovmf_vars_path` — обязательные
//! top-level поля файла, не вложенные опциональные секции, и пользователь
//! не может создать инстанс, не задумываясь о том, куда лягут его файлы.

use std::path::{Path, PathBuf};

use andler_core::{
    AudioConfig, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig, GpuConfig, InputConfig,
    MemoryConfig, NetworkConfig,
};
use andler_rpc::proto::CreateInstanceRequest;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum InstanceFileError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path} as TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Зеркало `CreateInstanceRequest` для TOML-файла. Поля, общие с
/// доменными типами (`cpu`, `memory`, ...), переиспользуют
/// `andler_core::config::*` напрямую через `Deserialize` — не отдельные
/// CLI-специфичные копии тех же полей, чтобы формат файла не мог
/// разойтись с тем, что реально принимает `InstanceConfig`.
#[derive(Debug, Deserialize)]
pub struct InstanceFile {
    pub name: String,
    pub iso_path: PathBuf,
    pub disk_path: PathBuf,
    /// Размер диска в GiB (не байтах — удобнее для ручного редактирования
    /// файла, аналогично `overlay_size_gib` в `Command::CreateAndroid`).
    /// Отсутствие ⇒ `DiskConfig::reference_default` оставляет 40 GiB.
    #[serde(default)]
    pub disk_size_gib: Option<u64>,
    pub ovmf_vars_path: PathBuf,

    #[serde(default)]
    pub cpu: Option<CpuConfig>,
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
    #[serde(default)]
    pub display: Option<DisplayConfig>,
    #[serde(default)]
    pub gpu: Option<GpuConfig>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub audio: Option<AudioConfig>,
    #[serde(default)]
    pub input: Option<InputConfig>,
}

impl InstanceFile {
    /// Читает и парсит TOML-файл по указанному пути. Ошибки несут путь
    /// файла в тексте (`InstanceFileError::Read`/`Parse`) — единственный
    /// файл, с которым работает один вызов `andler create`, но явный путь
    /// в сообщении дешевле, чем заставлять пользователя сопоставлять
    /// голую ошибку `toml::de::Error` с файлом, который он только что
    /// указал в `--file`.
    pub fn load(path: &Path) -> Result<Self, InstanceFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| InstanceFileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| InstanceFileError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Собирает `CreateInstanceRequest`, применяя `reference_default()`
    /// для любой не указанной в файле секции. Возвращает proto-тип прямо
    /// (не доменный `InstanceConfig`) — звонящему (`main.rs`) нужен
    /// именно `CreateInstanceRequest` для вызова
    /// `client.create_instance(...)`; конвертация в `InstanceConfig`
    /// происходит на стороне `andlerd` (`andler_rpc::convert`), CLI не
    /// должен повторять эту логику только для того, чтобы тут же
    /// конвертировать обратно в proto для отправки по сети.
    pub fn into_request(self) -> CreateInstanceRequest {
        let mut disk = DiskConfig::reference_default(self.disk_path.clone());
        if let Some(gib) = self.disk_size_gib {
            disk.size_bytes = gib * DiskConfig::GIB;
        }

        CreateInstanceRequest {
            name: self.name,
            iso_path: path_to_string(&self.iso_path),
            cpu: Some(
                self.cpu
                    .unwrap_or_else(CpuConfig::reference_default)
                    .into(),
            ),
            memory: Some(
                self.memory
                    .unwrap_or_else(MemoryConfig::reference_default)
                    .into(),
            ),
            disk: Some(disk.into()),
            display: Some(
                self.display
                    .unwrap_or_else(DisplayConfig::reference_default)
                    .into(),
            ),
            gpu: Some(self.gpu.unwrap_or_else(GpuConfig::reference_default).into()),
            network: Some(
                self.network
                    .unwrap_or_else(NetworkConfig::reference_default)
                    .into(),
            ),
            firmware: Some(
                FirmwareConfig::reference_default(self.ovmf_vars_path.clone()).into(),
            ),
            audio: Some(
                self.audio
                    .unwrap_or_else(AudioConfig::reference_default)
                    .into(),
            ),
            input: Some(
                self.input
                    .unwrap_or_else(InputConfig::reference_default)
                    .into(),
            ),
        }
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Минимальный валидный TOML — только обязательные top-level поля,
    /// все секции отсутствуют. Главное, что должно быть проверено: файл
    /// без единой секции `[cpu]`/`[memory]`/... всё равно даёт полностью
    /// заполненный `CreateInstanceRequest` (через `reference_default()`
    /// каждой секции), а не `None`-поля, которые `andlerd` отверг бы как
    /// `MissingField`.
    const MINIMAL_TOML: &str = r#"
        name = "test-vm"
        iso_path = "/tmp/test.iso"
        disk_path = "/tmp/disk.qcow2"
        ovmf_vars_path = "/tmp/test_VARS.fd"
    "#;

    #[test]
    fn minimal_file_parses_and_fills_every_section_via_defaults() {
        let file: InstanceFile = toml::from_str(MINIMAL_TOML).expect("minimal TOML must parse");
        let request = file.into_request();

        assert_eq!(request.name, "test-vm");
        assert_eq!(request.iso_path, "/tmp/test.iso");
        // Каждое поле — Some, не None: отсутствие секции в файле не должно
        // протекать как "поле не указано" в proto-запрос, иначе andlerd
        // отверг бы его как ConvertError::MissingField несмотря на то,
        // что файл сам по себе валиден с точки зрения CLI.
        assert!(request.cpu.is_some());
        assert!(request.memory.is_some());
        assert!(request.display.is_some());
        assert!(request.gpu.is_some());
        assert!(request.network.is_some());
        assert!(request.firmware.is_some());
        assert!(request.audio.is_some());
        assert!(request.input.is_some());

        let disk = request.disk.expect("disk must be Some");
        assert_eq!(disk.path, "/tmp/disk.qcow2");
        // 40 GiB — DiskConfig::reference_default без disk_size_gib override.
        assert_eq!(disk.size_bytes, 40 * 1024 * 1024 * 1024);
    }

    #[test]
    fn disk_size_gib_overrides_default_size_bytes() {
        let toml = format!("{MINIMAL_TOML}\ndisk_size_gib = 100\n");
        let file: InstanceFile = toml::from_str(&toml).expect("TOML must parse");
        let request = file.into_request();

        let disk = request.disk.expect("disk must be Some");
        assert_eq!(disk.size_bytes, 100 * 1024 * 1024 * 1024);
    }

    #[test]
    fn explicit_cpu_section_overrides_default() {
        let toml = format!(
            "{MINIMAL_TOML}\n[cpu]\ncores = 8\nsockets = 1\nthreads = 2\naffinity = []\npriority = \"High\"\n"
        );
        let file: InstanceFile = toml::from_str(&toml).expect("TOML with [cpu] must parse");
        let request = file.into_request();

        let cpu = request.cpu.expect("cpu must be Some");
        assert_eq!(cpu.cores, 8);
        assert_eq!(cpu.sockets, 1);
        assert_eq!(cpu.threads, 2);
    }

    #[test]
    fn explicit_gpu_passthrough_round_trips_through_request() {
        // RenderBackend::Passthrough — struct-вариант (несёт gpu_pci_id),
        // в отличие от Venus/VirtioGpu/VirGl/Cpu — отдельный тест на то,
        // что serde действительно парсит inline-таблицу TOML для этого
        // варианта (`{ Passthrough = { gpu_pci_id = "..." } }`), не только
        // unit-варианты.
        let toml = format!(
            "{MINIMAL_TOML}\n[gpu]\nhostmem_bytes = 1073741824\nblob = true\ngl = true\n\
             [gpu.render_backend.Passthrough]\ngpu_pci_id = \"0000:01:00.0\"\n"
        );
        let file: InstanceFile = toml::from_str(&toml).expect("TOML with GPU passthrough must parse");
        let request = file.into_request();

        let gpu = request.gpu.expect("gpu must be Some");
        let render_backend = gpu.render_backend.expect("render_backend must be Some");
        match render_backend.kind {
            Some(andler_rpc::proto::render_backend::Kind::Passthrough(p)) => {
                assert_eq!(p.gpu_pci_id, "0000:01:00.0");
            }
            other => panic!("expected Passthrough render backend, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_field_fails_to_parse() {
        let toml = r#"
            name = "test-vm"
            iso_path = "/tmp/test.iso"
        "#;
        // disk_path/ovmf_vars_path отсутствуют — должно провалиться на
        // парсинге, не на отправке запроса демону: пользователь должен
        // узнать о проблеме файла сразу, не после round-trip по сети.
        let result: Result<InstanceFile, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn load_reports_read_error_for_missing_file() {
        let err = InstanceFile::load(Path::new("/nonexistent/path/instance.toml"))
            .expect_err("loading a missing file must fail");
        assert!(matches!(err, InstanceFileError::Read { .. }));
    }

    #[test]
    fn load_reports_parse_error_for_invalid_toml() {
        let dir = std::env::temp_dir().join(format!(
            "andler-cli-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("invalid.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let err = InstanceFile::load(&path).expect_err("invalid TOML must fail to parse");
        assert!(matches!(err, InstanceFileError::Parse { .. }));

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
