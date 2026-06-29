# План улучшений CLI и Daemon

## 1. Логирование в andlerd

### Проблема
При запуске `./andlerd` пользователь видит только одну строку `andlerd: listening on 127.0.0.1:50051` и всё. Нет информации о:
- Какие инстансы восстановлены из БД при старте
- Какие инстансы создаются/запускаются/останавливаются
- Какие QEMU-процессы запускаются
- Какие ошибки 발생ают (если нет `RUST_LOG`)
- Какие GPU обнаружены
- Какие snapshot-операции выполняются

Единственный `info!`-вызов в проекте — в `main.rs:71` ("restored instances from store"), но он тоже не выводится потому что `EnvFilter::from_default_env()` без `RUST_LOG` фильтрует на `warn`.

### Что надо
Чтобы `andlerd` выводил в stderr логи всех ключевых операций на уровне `info` по умолчанию, без необходимости выставлять `RUST_LOG`. Пользователь должен видеть что происходит при старте, при создании/запуске/остановке VM, при snapshot-операциях, при обнаружении GPU.

### Решение
**Файл `daemon/src/main.rs`** — изменить дефолт фильтра:
```rust
// Было:
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();

// Стало:
let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .init();
```

**Файл `daemon/src/daemon/instance_ops.rs`** — добавить `tracing::info!` в каждый метод:
- `create_instance`: `"created instance {id} ({kind_name})"`
- `start_instance`: `"starting instance {id}"`
- `stop_instance`: `"stopping instance {id} (graceful={graceful})"`
- `pause_instance`: `"pausing instance {id}"`
- `resume_instance`: `"resuming instance {id}"`
- `remove_instance`: `"removing instance {id} (purge={purge})"`

**Файл `daemon/src/daemon/snapshot_ops.rs`** — добавить `tracing::info!`:
- `create_snapshot`: `"creating snapshot tag={tag} for instance {id}"`
- `restore_snapshot`: `"restoring snapshot tag={tag} for instance {id}"`
- `delete_snapshot`: `"deleting snapshot tag={tag} for instance {id}"`

**Файл `backends/andler-qemu/src/backend.rs`** — добавить `tracing::info!`:
- `spawn`: `"spawning QEMU for instance {id}, render={render_backend}"`
- `stop`: `"stopping QEMU for instance {id}"`
- `pause`: `"pausing QEMU for instance {id}"`
- `resume`: `"resuming QEMU for instance {id}"`

**Файл `backends/andler-qemu/src/gpu_metrics.rs`** — добавить `tracing::debug!` при обнаружении GPU vendor.

**Результат при запуске без RUST_LOG:**
```
info: restored instances from store (2 instances)
info: andlerd: listening on 127.0.0.1:50051
info: starting instance abc123-def456
info: spawning QEMU for instance abc123-def456, render=Venus
info: creating snapshot tag=snap1 for instance abc123-def456
info: stopping instance abc123-def456 (graceful=true)
```

---

## 2. OVMF VARS по умолчанию

### Проблема
При создании Linux VM через CLI пользователь ОБЯЗАН указывать `--ovmf-vars-template`, хотя путь стандартный: `/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd`. Это лишний аргумент в 99% случаев. Пользователь не понимает зачем это нужно и что это за "VARS".

В start.sh это решается автоматически — скрипт копирует шаблон в текущую директорию.

### Что надо
Чтобы `--ovmf-vars-template` имел дефолтное значение и не был обязателен. Если пользователь не указывает путь — используется стандартный системный шаблон.

### Решение
**Файл `core/andler-core/src/config/firmware.rs`** — вынести дефолты в константы:
```rust
pub const DEFAULT_OVMF_CODE: &str = "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd";
pub const DEFAULT_OVMF_VARS_TEMPLATE: &str = "/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd";
```

**Файл `cli/src/main.rs`** — изменить CLI-аргумент:
```rust
// Было:
#[arg(long)]
ovmf_vars_template: Option<String>,

// Стало:
#[arg(long, default_value = DEFAULT_OVMF_VARS_TEMPLATE)]
ovmf_vars_template: String,
```

**Файл `cli/src/create.rs`** — убрать проверку `err_exit` для `ovmf_vars_template`:
```rust
// Было:
let ovmf = ovmf_vars_template
    .unwrap_or_else(|| err_exit("error: --ovmf-vars-template is required"));

// Стало:
// ovmf_vars_template уже имеет дефолтное значение, unwrap не нужен
```

**Файл `cli/src/instance_file.rs`** — если `ovmf_vars_path` не указан в TOML, использовать дефолт:
```rust
pub ovmf_vars_path: Option<PathBuf>,  // было PathBuf (обязательный)

// В into_request():
let ovmf_path = self.ovmf_vars_path
    .unwrap_or_else(|| PathBuf::from(FirmwareConfig::DEFAULT_OVMF_VARS_TEMPLATE));
```

---

## 3. ISO path опционален для Linux

### Проблема
При создании Linux VM через `--kind linux` пользователь ОБЯЗАН указывать `--iso-path`, даже если у него уже есть готовый диск. Это неудобно если:
- Пользователь уже установил ОС и просто хочет запустить VM
- Пользователь импортировал диск извне
- Пользователь клонировал инстанс

### Что надо
Чтобы `--iso-path` был опциональным. Если указан только `--disk-path` без ISO — VM создаётся с готовым диском без установки. Если не указан ни ISO ни диск — ошибка.

### Решение
**Файл `cli/src/create.rs`** — изменить валидацию:
```rust
// Было:
let iso = iso_path
    .unwrap_or_else(|| err_exit("error: --iso-path is required for --kind linux"));
let disk = disk_path
    .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));

// Стало:
let disk = disk_path
    .unwrap_or_else(|| err_exit("error: --disk-path is required for --kind linux"));
let iso = iso_path.unwrap_or_default();  // пустая строка если не указан
```

**Файл `cli/src/create.rs`** — в `build_linux_request` передавать `iso_path` как пустую строку:
```rust
fn build_linux_request(
    name: String,
    iso_path: String,      // может быть пустой
    disk_path: String,
    disk_size_gib: Option<u64>,
    ovmf_vars_template: String,
) -> CreateInstanceRequest {
    // ...
    CreateInstanceRequest {
        name,
        iso_path,  // пустая строка = нет ISO
        // ...
    }
}
```

**Файл `backends/andler-qemu/src/cmdline.rs`** — если `iso_path` пустой, не генерировать IDE-CD:
```rust
fn disk_args(cfg: &InstanceConfig) -> Vec<String> {
    // ... disk args ...

    // CD-ROM только если есть ISO
    if !cfg.iso_path.is_empty() {
        args.push("-drive".to_string());
        args.push(format!("file={},if=none,id=drive-cd0,format=raw,readonly=on", cfg.iso_path));
        args.push("-device".to_string());
        args.push("ide-cd,drive=drive-cd0,id=cd0,bootindex=2".to_string());
    }

    args
}
```

---

## 4. Boot priority (приоритет загрузки)

### Проблема
Нет возможности менять приоритет загрузки. В QEMU это делается через `-boot menu=on` + `bootindex=N` на устройствах. Сейчас хардкод: disk=1, cdrom=2. Пользователь не может:
- Загрузиться с CD-ROM для установки ОС
- Загрузиться по сети (PXE)
- Изменить порядок без редактирования TOML

### Что надо
Добавить флаг `--boot-order` в CLI и секцию `boot` в TOML. Варианты:
- `disk` — диск первый, CD-ROM второй (дефолт, текущее поведение)
- `cdrom` — CD-ROM первый, диск второй (для установки ОС)
- `network` — сеть первая, диск второй (для PXE-загрузки)

### Решение
**Новый файл `core/andler-core/src/config/boot.rs`**:
```rust
use serde::{Deserialize, Serialize};

/// Приоритет загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootOrder {
    /// Disk first, CD-ROM second (default).
    Disk,
    /// CD-ROM first, disk second (for OS installation).
    Cdrom,
    /// Network first, disk second (for PXE boot).
    Network,
}

impl Default for BootOrder {
    fn default() -> Self {
        BootOrder::Disk
    }
}
```

**Файл `core/andler-core/src/config/instance.rs`** — добавить поле:
```rust
pub struct InstanceConfig {
    // ... существующие поля ...
    #[serde(default)]
    pub boot_order: BootOrder,
}
```

**Файл `backends/andler-qemu/src/cmdline.rs`** — использовать `boot_order`:
```rust
fn disk_args(cfg: &InstanceConfig, qmp_socket_path: &Path) -> Vec<String> {
    // ... disk device ...
    let disk_bootindex = match cfg.boot_order {
        BootOrder::Disk | BootOrder::Network => 1,
        BootOrder::Cdrom => 2,
    };
    args.push(format!(
        "virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex={disk_bootindex},num-queues=4"
    ));

    // ... cdrom device ...
    if !cfg.iso_path.is_empty() {
        let cdrom_bootindex = match cfg.boot_order {
            BootOrder::Cdrom => 1,
            BootOrder::Disk | BootOrder::Network => 2,
        };
        args.push(format!(
            "ide-cd,drive=drive-cd0,id=cd0,bootindex={cdrom_bootindex}"
        ));
    }
    // ...
}
```

**Файл `cli/src/main.rs`** — добавить флаг:
```rust
#[arg(long, value_enum, default_value_t = CliBootOrder::Disk)]
boot_order: CliBootOrder,
```

**Файл `cli/src/create.rs`** — передавать `boot_order` в запрос.

**Файл `cli/src/instance_file.rs`** — добавить в TOML:
```rust
#[serde(default)]
pub boot_order: Option<String>,  // "disk", "cdrom", "network"
```

---

## 5. Partial instance ID

### Проблема
Пользователь ОБЯЗАН указывать полный UUID инстанса (36 символов). Это неудобно:
- UUID генерируется автоматически и пользователь его не знает
- Приходится копировать из вывода `andler list`
- Docker позволяет указывать первые символы UUID

### Что надо
Чтобы все команды принимали prefix UUID (минимум 4 символа) (ai написал что минимум 4 символа почему так, в docker можно 2 символа точно указать или даже 1 я бы хотел чтобы также можно было). Если prefix однозначен — используется. Если несколько инстансов match'ятся — ошибка с подсказкой.

### Решение
**Файл `daemon/src/daemon/error.rs`** — добавить вариант:
```rust
pub enum DaemonError {
    // ... существующие варианты ...

    /// Multiple instances match the given prefix.
    #[error("ambiguous prefix '{0}': matches {1:?}")]
    AmbiguousPrefix(String, Vec<InstanceId>),

    /// Prefix is too short (less than 4 characters).
    #[error("prefix '{0}' is too short, use at least 4 characters")]
    PrefixTooShort(String),
}
```

**Файл `daemon/src/daemon/mod.rs`** — добавить метод:
```rust
impl Daemon {
    /// Находит инстанс по prefix UUID (минимум 4 символа).
    /// Возвращает ошибку если prefix слишком короткий, не найден, или неоднозначен.
    pub async fn find_instance_by_prefix(&self, prefix: &str) -> Result<InstanceId, DaemonError> {
        if prefix.len() < 4 {
            return Err(DaemonError::PrefixTooShort(prefix.to_string()));
        }

        let instances = self.instances.read().await;
        let matches: Vec<InstanceId> = instances.keys()
            .filter(|id| id.0.starts_with(prefix))
            .copied()
            .collect();

        match matches.len() {
            0 => Err(DaemonError::InstanceNotFound(InstanceId::from_str(prefix))),
            1 => Ok(matches[0]),
            _ => Err(DaemonError::AmbiguousPrefix(prefix.to_string(), matches)),
        }
    }
}
```

**Файл `daemon/src/service.rs`** — использовать `find_instance_by_prefix`:
```rust
// Было:
let id = InstanceId::from_str(&request.instance_id)?;

// Стало:
let id = daemon.find_instance_by_prefix(&request.instance_id).await?;
```

**Важно:** `InstanceId::from_str` должен поддерживать prefix — сейчас он ожидает полный UUID. Нужно изменить `from_str` чтобы он принимал prefix как `InstanceId(prefix.to_string())`.

---

## 6. NVIDIA metrics (nvidia-smi парсинг)

### Проблема
`andler metrics <id>` не выводит GPU-метрики для NVIDIA. Причина: `read_nvidia_metrics()` возвращает `None` если nvidia-smi выводит что-то неожиданное:
- `[Not Supported]` для некоторых полей
- `[N/A]` или `N/A`
- Пустые строки
- Дополнительные строки (заголовок,-footer)

### Что надо
Чтобы `andler metrics` показывал VRAM и GPU load для NVIDIA. Если конкретное поле не поддерживается — показывать `N/A` для него, а не терять все метрики.

### Решение
**Файл `backends/andler-qemu/src/gpu_metrics.rs`** — улучшить `read_nvidia_metrics()`:
```rust
fn read_nvidia_metrics() -> Option<ResourceMetrics> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        tracing::warn!("nvidia-smi failed with exit code: {:?}", output.status.code());
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Фильтруем пустые строки и ищем строку с данными
    let line = stdout.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;

    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 3 {
        tracing::warn!("nvidia-smi: expected 3 fields, got {}: {:?}", parts.len(), line);
        return None;
    }

    // Обрабатываем "Not Supported", "N/A", "[N/A]" как None
    let vram_used_mib = parse_nvidia_field(&parts[0]);
    let vram_total_mib = parse_nvidia_field(&parts[1]);
    let gpu_load = parse_nvidia_field_f32(&parts[2]);

    Some(ResourceMetrics {
        vram_used_bytes: vram_used_mib.map(|v| v * 1024 * 1024),
        vram_total_bytes: vram_total_mib.map(|v| v * 1024 * 1024),
        gpu_load_percent: gpu_load.map(|v| v.clamp(0.0, 100.0)),
        ..ResourceMetrics::default()
    })
}

/// Парсит поле nvidia-smi: возвращает None для "Not Supported", "N/A", "[N/A]".
fn parse_nvidia_field(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() || s == "[Not Supported]" || s == "N/A" || s == "[N/A]" {
        return None;
    }
    s.parse().ok()
}

fn parse_nvidia_field_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() || s == "[Not Supported]" || s == "N/A" || s == "[N/A]" {
        return None;
    }
    s.parse().ok()
}
```

---

## 7. disk create: аргументы и документация

### Проблема
1. Порядок аргументов `andler disk create 64GB disk.qcow2` неочевиден — пользователь ожидает `--size` как named arg
2. Если указать `andler disk create --size 64GB disk` (без расширения) — создаётся файл `disk` без `.qcow2`
3. `disk info` показывает мало информации — нет format, dirty, refcount

### Что надо
1. Порядок `--size` как named arg, путь как позиционный: `andler disk create --size 64GB ~/my-disk`
2. Автоматическое добавление `.qcow2` если расширение не указано
3. `disk info` показывает всё: format, virtual_size, actual_size, backing_file, dirty, refcount

### Решение
**Файл `cli/src/main.rs`** — поменять порядок аргументов:
```rust
// Было:
Create {
    path: PathBuf,
    #[arg(long)]
    size: String,
},

// Стало:
Create {
    /// Disk size (e.g. "64GB", "128000MB", "1T", or plain bytes).
    #[arg(long)]
    size: String,
    /// Path for the new disk file.
    path: PathBuf,
},
```

**Файл `cli/src/disk.rs`** — авто-добавление `.qcow2`:
```rust
DiskAction::Create { size, mut path } => {
    // Авто-добавление .qcow2 если расширение не указано
    if path.extension().is_none() {
        path.set_extension("qcow2");
    }
    let bytes = parse_size(&size)?;
    andler_disk::qcow2::create(&path, bytes).await?;
    println!("created {} ({})", path.display(), format_size(bytes));
}
```

**Файл `services/andler-disk/src/qcow2.rs`** — расширить `DiskInfo`:
```rust
pub struct DiskInfo {
    pub format: String,
    pub virtual_size: u64,
    pub actual_size: u64,
    pub backing_file: Option<String>,
    pub dirty: bool,           // новый
    pub refcount: Option<u32>, // новый
}
```

**Файл `cli/src/disk.rs`** — выводить больше информации:
```rust
DiskAction::Info { path } => {
    let info = andler_disk::qcow2::info(&path).await?;
    println!("path:         {}", path.display());
    println!("format:       {}", info.format);
    println!("virtual_size: {}", format_size(info.virtual_size));
    println!("actual_usage: {} ({:.1}%)", format_size(info.actual_size), pct);
    if let Some(bf) = &info.backing_file {
        println!("backing_file: {bf}");
    }
    println!("dirty:        {}", info.dirty);
    if let Some(rc) = info.refcount {
        println!("refcount:     {rc}");
    }
}
```

---

## 8. CLI flags для GPU/display и других параметров

### Проблема
При создании VM через CLI (`--kind linux`) невозможно выбрать:
- Render backend (venus/virtiogpu/virgl/cpu)
- Display engine (sdl/spice/dbus/none)
- Разрешение, DPI, FPS limit
- Количество ядер, объём памяти
- Тип сети, bridge interface
- Включить balloon/zram/ksm

Единственный способ — через TOML файл, что неудобно для быстрого создания.

### Что надо
Добавить CLI-флаги для всех параметров которые можно задать в TOML. Дефолты — те же что в `reference_default()`.

### Решение
**Файл `cli/src/main.rs`** — добавить флаги в `Command::Create`:
```rust
Create {
    // ... существующие флаги ...

    // GPU
    #[arg(long, value_enum, default_value_t = CliRenderBackend::Venus)]
    render: CliRenderBackend,
    #[arg(long, value_enum, default_value_t = CliDisplayEngine::Sdl)]
    display: CliDisplayEngine,
    #[arg(long)]
    hostmem: Option<String>,     // "4G", "4096M"
    #[arg(long)]
    resolution: Option<String>,  // "1920x1080"
    #[arg(long)]
    dpi: Option<u32>,
    #[arg(long)]
    fps_limit: Option<u32>,
    #[arg(long)]
    fullscreen: bool,

    // CPU
    #[arg(long, default_value_t = 4)]
    cores: u32,
    #[arg(long, default_value_t = 1)]
    sockets: u32,
    #[arg(long, default_value_t = 1)]
    threads: u32,
    #[arg(long, value_enum, default_value_t = CliCpuPriority::Normal)]
    cpu_priority: CliCpuPriority,

    // Memory
    #[arg(long, default_value = "4G")]
    memory: String,
    #[arg(long)]
    balloon: bool,
    #[arg(long)]
    zram: bool,

    // Network
    #[arg(long, value_enum, default_value_t = CliNetworkMode::Nat)]
    net: CliNetworkMode,
}
```

**Файлы enum'ов** — добавить в `cli/src/main.rs`:
```rust
#[derive(Clone, Copy, ValueEnum)]
enum CliRenderBackend {
    Venus,
    Virtiogpu,
    Virgl,
    Cpu,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliDisplayEngine {
    Sdl,
    Spice,
    Dbus,
    None,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliNetworkMode {
    Nat,
    Isolated,
}
```

**Файл `cli/src/create.rs`** — переопределять `reference_default()`:
```rust
fn build_linux_request(/* ... */, args: &CreateArgs) -> CreateInstanceRequest {
    let mut gpu = andler_core::GpuConfig::reference_default();
    gpu.render_backend = match args.render {
        CliRenderBackend::Venus => andler_core::RenderBackend::Venus,
        CliRenderBackend::Virtiogpu => andler_core::RenderBackend::VirtioGpu,
        CliRenderBackend::VirGl => andler_core::RenderBackend::VirGl,
        CliRenderBackend::Cpu => andler_core::RenderBackend::Cpu,
    };
    if let Some(ref hostmem) = args.hostmem {
        gpu.hostmem_bytes = parse_size(hostmem).unwrap();
    }

    let mut display = andler_core::DisplayConfig::reference_default();
    display.display_engine = match args.display {
        CliDisplayEngine::Sdl => andler_core::DisplayEngine::Sdl,
        CliDisplayEngine::Spice => andler_core::DisplayEngine::Spice,
        CliDisplayEngine::Dbus => andler_core::DisplayEngine::Dbus,
        CliDisplayEngine::None => andler_core::DisplayEngine::None,
    };
    // ... разрешение, DPI, FPS ...

    // ... остальные секции ...
}
```

---

## Порядок реализации

| # | Задача | Сложность | Время |
|---|--------|-----------|-------|
| 1 | Логирование | Простая | 30 мин |
| 2 | OVMF VARS по умолчанию | Простая | 15 мин |
| 3 | ISO path опционален | Средняя | 45 мин |
| 4 | disk create порядок аргументов | Простая | 15 мин |
| 5 | Partial instance ID | Средняя | 1 час |
| 6 | Boot priority | Средняя | 1.5 часа |
| 7 | CLI flags GPU/display | Сложная | 2-3 часа |
| 8 | NVIDIA metrics | Средняя | 1 час |
