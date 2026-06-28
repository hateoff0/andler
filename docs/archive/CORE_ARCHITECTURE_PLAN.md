# ANDLER Core — План архитектуры

> **Статус:** подробный технический план на основе рабочего прототипа.
>
> **ANDLER** = *Android Linux Emulator & Runtime*.
> **Язык:** Rust (workspace)

---

## 1. Обзор

Core-часть ANDLER — это демон (`andlerd`) и CLI-клиент (`andler`), которые управляют виртуализацией через обёртку над QEMU. Архитектура основана на реальной рабочей конфигурации с GPU-ускорением через Venus.

### 1.1 Ключевые компоненты
- **Backend-абстракция** — единый интерфейс для разных гипервизоров (QEMU, rust-vmm)
- **Управление ресурсами** — CPU, RAM, GPU, диск, сеть
- **gRPC API** — единый интерфейс для CLI и GUI
- **Мониторинг** — метрики в реальном времени

---

## 2. Backend-абстракция

### 2.1 Trait HypervisorBackend

```rust
#[async_trait]
pub trait HypervisorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn supported_render_backends(&self) -> &[RenderBackend];
    
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>;
    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError>;
    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError>;
    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>;
    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>;
    async fn snapshot(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>;
    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>;
}
```

> **Контракт для незавершённых backend'ов** (`andler-vmm` на старте, а также `RenderBackend::Passthrough` внутри `andler-qemu`): любой метод, который backend не умеет выполнить на данном этапе, должен возвращать `Err(BackendError::NotImplemented { backend, operation })`, а не паниковать через `todo!()`/`unimplemented!()`. `andler-daemon` транслирует эту ошибку в штатный gRPC-статус (`UNIMPLEMENTED`), а не в краш демона. Это позволяет регистрировать backend в реестре до его полной готовности.

### 2.2 Реализация для QEMU

**Ключевые параметры запуска (на основе start.sh):**

| Параметр | Значение | Описание |
|---|---|---|
| Machine | `q35,accel=kvm,usb=on` | Современный тип машины с USB |
| CPU | `host,kvm=on,+topoext,migratable=no` | Хост-процессор с KVM |
| RAM | `8G` с `memory-backend-memfd` | Разделяемая память для KSM |
| GPU | `virtio-gpu-gl,hostmem=4096M,blob=true,venus=true` | Venus-контекст для Vulkan |
| Display | `sdl,gl=on,show-cursor=off` | SDL с OpenGL |
| Disk | `qcow2,discard=on,detect-zeroes=on,aio=threads` | Оптимизированный диск |
| Network | `user,model=virtio-net-pci` | Пользовательский режим |
| Audio | `pipewire` + `hda-output` | Современный звук |
| Input | `virtio-tablet-pci` + `virtserialport` | Сенсорный ввод и буфер обмена |

### 2.3 GPU-конфигурация

**Варианты рендеринга (в скоупе текущей реализации):**
- **Venus** — `virtio-gpu-gl,venus=true` (максимальная производительность Vulkan)
- **VirtIO-GPU** — `virtio-gpu-pci` (универсальный вариант)
- **CPU** — `vga std` (программный рендеринг)

**Ключевые параметры GPU:**
- `hostmem` — выделенная память для GPU (рекомендуется 4096M для игр)
- `blob=true` — поддержка blob-модели для Venus
- `gl=on` — включение OpenGL для совместимости

> **Вне скоупа на этом этапе:** `RenderBackend::Passthrough` (полный проброс физической GPU через VFIO) не реализуется. Вариант остаётся в enum как зарезервированный (см. §4.1), чтобы не менять публичный API позже, но backend `andler-qemu` должен возвращать понятную ошибку `BackendError::NotImplemented` при попытке его использовать, а не пытаться собрать VFIO cmdline. Причина отказа на этом этапе: VFIO требует IOMMU-группировки, отдельной настройки на хосте (vfio-pci binding, ACS override) и тестирования на multi-GPU системах — отдельная большая задача, не блокирующая MVP.

### 2.4 Референсная реализация (start.sh)

Скрипт `start.sh` служит рабочей референсной реализацией конфигурации QEMU:

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

VM_NAME="linux"
RAM_SIZE="8G"
VRAM_SIZE="4096M"
DISK_SIZE="40G"

DISK_IMG="$SCRIPT_DIR/disk.qcow2"
ISO_IMG="$SCRIPT_DIR/cachyos-desktop-linux-260426.iso"

OVMF_CODE="/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd"
OVMF_VARS_TEMPLATE="/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd"
OVMF_VARS="$SCRIPT_DIR/${VM_NAME}_VARS.fd"

RESET_BOOT=false
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --reset-boot) RESET_BOOT=true; shift ;;
        *) ARGS+=("$1"); shift ;;
    esac
done

if [ ! -f "$OVMF_CODE" ]; then
    exit 1
fi

if [ "$RESET_BOOT" = true ] && [ -f "$OVMF_VARS" ]; then
    rm -f "$OVMF_VARS"
fi

if [ ! -f "$OVMF_VARS" ]; then
    cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
fi

if [ ! -f "$DISK_IMG" ]; then
    qemu-img create -f qcow2 "$DISK_IMG" "$DISK_SIZE"
fi

exec qemu-system-x86_64 \
  -name "$VM_NAME",process="$VM_NAME" \
  -machine q35,accel=kvm,usb=on \
  -cpu host,kvm=on,+topoext,migratable=no \
  -smp cpus=4,sockets=1,dies=1,cores=4,threads=1 \
  -m "$RAM_SIZE" \
  -object memory-backend-memfd,id=mem1,size="$RAM_SIZE",share=on \
  -machine memory-backend=mem1 \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$OVMF_VARS" \
  -vga none \
  -device virtio-gpu-gl,hostmem="$VRAM_SIZE",blob=true,venus=true \
  -display sdl,gl=on,show-cursor=off \
  -drive file="$DISK_IMG",format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads \
  -device virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4 \
  -drive file="$ISO_IMG",media=cdrom,if=none,id=drive-cd0 \
  -device ide-cd,drive=drive-cd0,id=cd0,bootindex=2 \
  -device virtio-tablet-pci,id=tablet0 \
  -device virtio-serial-pci \
  -device virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0 \
  -chardev qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on \
  -nic user,model=virtio-net-pci \
  -audiodev pipewire,id=snd0 \
  -device ich9-intel-hda -device hda-output,audiodev=snd0 \
  -boot menu=on "${ARGS[@]}"
```

**Ключевые параметры из start.sh:**
- **CPU**: `host,kvm=on,+topoext,migratable=no` — хост-процессор с KVM
- **RAM**: `8G` с `memory-backend-memfd` — разделяемая память для KSM
- **GPU**: `virtio-gpu-gl,hostmem=4096M,blob=true,venus=true` — Venus-контекст
- **Display**: `sdl,gl=on,show-cursor=off` — SDL с OpenGL
- **Disk**: `qcow2,discard=on,detect-zeroes=on,aio=threads` — оптимизированный диск
- **Network**: `user,model=virtio-net-pci` — пользовательский режим
- **Audio**: `pipewire` + `hda-output` — современный звук
- **Input**: `virtio-tablet-pci` + `virtserialport` — сенсорный ввод и буфер обмена

---

## 3. Структура workspace

```
andler/
├── Cargo.toml
├── crates/
│   ├── andler-core/         # Домен: Instance, FSM, конфиги, trait HypervisorBackend
│   ├── andler-qemu/         # Backend: обёртка над QEMU-процессом + QMP-клиент
│   ├── andler-vmm/          # Backend-заготовка под rust-vmm (на будущее)
│   ├── andler-disk/         # qcow2: создание/clone/resize/compact
│   ├── andler-net/          # Конфигурация сети: NAT/bridge/изоляция
│   ├── andler-store/        # Персистентность (sqlite)
│   ├── andler-rpc/          # gRPC-схема (proto) + сервер + клиент
│   ├── andler-daemon/       # Бинарь andlerd: сборка реестра backend'ов
│   └── andler-cli/          # Бинарь andler: команды, вызовы через andler-rpc
├── guest-image/             # Сборка гостевых образов (не Rust-крейт)
├── tests/
│   ├── integration/
│   └── e2e/
└── docker/
    ├── Dockerfile.dev
    ├── Dockerfile.daemon
    └── docker-compose.yml
```

---

## 4. Доменная модель

### 4.1 Ключевые типы

```rust
pub struct InstanceId(Uuid);

pub enum InstanceKind {
    AndroidVm { android_profile: AndroidProfile },
    LinuxVm { iso_path: PathBuf },
}

pub enum RenderBackend {
    Venus,
    VirtioGpu,
    VirGl,
    Cpu,
    /// Зарезервировано на будущее (VFIO GPU passthrough).
    /// Не реализуется на текущем этапе — andler-qemu возвращает
    /// BackendError::NotImplemented. См. §2.3.
    Passthrough { gpu_pci_id: String },
}

pub struct InstanceConfig {
    pub id: InstanceId,
    pub name: String,
    pub kind: InstanceKind,
    pub backend: BackendKind,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,        // size, ballooning on/off, zram on/off
    pub disk: DiskConfig,            // path, size, format, base_image
    pub display: DisplayConfig,      // resolution, dpi, fps_limit, display_engine
    pub render_backend: RenderBackend,
    pub network: NetworkConfig,      // Nat | Bridge | Isolated
}

pub enum InstanceState {
    Created, Starting, Running, Paused, Stopping, Stopped,
    Error { message: String },
}
```

### 4.2 Конечный автомат состояний

```
Created → Starting → Running ⇄ Paused
                         │
                         ▼
                      Stopping → Stopped
                         │
                         ▼
                       Error
```

### 4.3 Конфигурация ресурсов

**CPU:**
```rust
pub struct CpuConfig {
    pub cores: u32,
    pub sockets: u32,
    pub threads: u32,
    pub affinity: Option<Vec<usize>>, // привязка к ядрам
    pub priority: CpuPriority,
}
```

**Memory:**
```rust
pub struct MemoryConfig {
    pub size: u64, // в байтах
    pub ballooning: bool,
    pub zram: bool,
    pub ksm: bool, // Kernel Samepage Merging
}
```

**Disk:**
```rust
pub struct DiskConfig {
    pub path: PathBuf,
    pub size: u64,
    pub format: DiskFormat, // qcow2, raw, vdi
    pub base_image: Option<PathBuf>, // для linked clones
    pub thin_provisioning: bool, // растить по мере использования
    pub trim_on_shutdown: bool, // автоматическая очистка
}
```

**Display:**
```rust
pub struct DisplayConfig {
    pub resolution: Resolution, // Width x Height
    pub dpi: u32,
    pub fps_limit: u32, // 30/60/90/120/unlimited
    pub display_engine: DisplayEngine, // sdl, spice, dbus
    pub fullscreen: bool,
}
```

### 4.4 Запуск Android-инстанса (AndroidVm)

Этот раздел описывает, что конкретно происходит между «пользователь нажал Создать → Android» и «у демона есть `InstanceConfig`, готовый к `spawn()`». Без этого раздела `InstanceKind::AndroidVm` в коде — пустой enum-вариант без реализации.

#### 4.4.1 Откуда берётся гостевая система

Android-инстанс — это **не отдельный тип VM на уровне QEMU**, а `InstanceKind::LinuxVm`-подобный запуск с заранее собранным гостевым образом, в котором уже настроен Waydroid (Уровень A, см. `GUEST_IMAGE_PLAN.md`). `andler-qemu` не отличает Android-инстанс от обычной VM на уровне cmdline — разница целиком на уровне того, **какой диск** подставляется и **что внутри него**.

```rust
pub struct AndroidProfile {
    pub android_version: AndroidVersion,   // напр. Android 11 / 13
    pub gapps: bool,
    pub microg: bool,
    pub libndk: bool,       // транслятор ARM -> x86 (libhoudini/libndk)
    pub root: RootMode,     // None | Magisk | KernelSU
}
```

#### 4.4.2 Жизненный цикл первого запуска

1. Пользователь выбирает `InstanceKind::AndroidVm { android_profile }` (через CLI/GUI/Presets).
2. Демон проверяет локальный кэш базовых образов (`~/.local/share/andler/guest-images/`) по ключу `(android_version, gapps, microg, libndk)`.
3. Если нужного базового образа нет — демон скачивает версионированный, подписанный артефакт из канала дистрибуции guest-image (см. `GUEST_IMAGE_PLAN.md`, артефакт `.qcow2`) и проверяет подпись/checksum перед использованием.
4. Демон создаёт **overlay-диск** (`qcow2` с `backing_file` на скачанный базовый образ, через `andler-disk`) — это и есть диск конкретного инстанса. Базовый образ никогда не модифицируется напрямую; это даёт даром: linked-clone семантику между несколькими Android-инстансами на одном базовом образе и OverlayFS-подобный Factory Reset (пересоздание overlay = чистый Android за секунды).
5. Демон строит `InstanceConfig` как для обычной `LinuxVm`: тот же `andler-qemu`, тот же набор устройств из §2.4 (start.sh), `DiskConfig.path` = путь к overlay-диску, `DiskConfig.base_image` = путь к базовому образу.
6. `RootMode`/Magisk-модули применяются не флагами QEMU, а через provisioning-шаг до первого запуска (монтирование overlay и копирование модулей) — это задача `andler-core`/`andler-disk`, не `andler-qemu`.

#### 4.4.3 Преобразование AndroidProfile → InstanceConfig

```rust
impl AndroidProfile {
    /// Резолвит профиль в конкретный путь к базовому образу
    /// (после скачивания/проверки кэша) и собирает итоговый
    /// InstanceConfig с overlay-диском.
    pub fn resolve(&self, base_image_path: PathBuf, opts: InstanceOpts) -> InstanceConfig {
        // RenderBackend и DisplayConfig берутся из общих
        // рекомендаций по ресурсам (см. §6.1 MASTER_PLAN, "Для игр"),
        // если не переопределены пресетом или пользователем.
        ...
    }
}
```

Это явная точка интеграции с `PRESETS_PLAN.md`: пресет (например, ANDLER-Proton под конкретную игру) применяется **после** `resolve()`, как частичный оверрайд готового `InstanceConfig` (rendering backend, keymap-метаданные, RAM/CPU), а не как замена самого механизма резолва образа.

#### 4.4.4 Зона ответственности по крейтам

| Шаг | Крейт |
|---|---|
| Скачивание/проверка базового образа | `andler-core` (HTTP-клиент + проверка подписи) или отдельный `andler-image-fetch` |
| Создание overlay-диска с `backing_file` | `andler-disk` |
| Provisioning root/Magisk перед первым стартом | `andler-disk` (монтирование overlay offline) |
| Сборка итогового `InstanceConfig` и cmdline | `andler-core` + `andler-qemu` (без изменений относительно LinuxVm) |
| Применение пресета | `andler-core` (оверрайд полей `InstanceConfig`) |

#### 4.4.5 Открытый вопрос

Формат и протокол доставки базового образа (собственный CDN/GitHub Releases/торрент для крупных образов) — решается в `GUEST_IMAGE_PLAN.md`, здесь зафиксирован только контракт: демон получает на вход путь к валидному `.qcow2`-файлу и не заботится о том, как он туда попал.

---

## 5. API/RPC

### 5.1 gRPC-методы

```protobuf
service AndlerService {
    rpc CreateInstance(CreateInstanceRequest) returns (InstanceInfo);
    rpc StartInstance(StartInstanceRequest) returns (StatusResponse);
    rpc StopInstance(StopInstanceRequest) returns (StatusResponse);
    rpc PauseInstance(PauseInstanceRequest) returns (StatusResponse);
    rpc ResumeInstance(ResumeInstanceRequest) returns (StatusResponse);
    rpc CloneInstance(CloneInstanceRequest) returns (InstanceInfo);
    rpc RemoveInstance(RemoveInstanceRequest) returns (StatusResponse);
    rpc ListInstances(ListInstancesRequest) returns (InstanceList);
    rpc GetInstanceStatus(GetInstanceStatusRequest) returns (InstanceInfo);
    rpc StreamInstanceLogs(StreamInstanceLogsRequest) returns (stream LogEntry);
    rpc StreamResourceMetrics(StreamResourceMetricsRequest) returns (stream ResourceMetrics);
}
```

### 5.2 Типы сообщений

```protobuf
message CreateInstanceRequest {
    string name = 1;
    InstanceKind kind = 2;
    BackendKind backend = 3;
    CpuConfig cpu = 4;
    MemoryConfig memory = 5;
    DiskConfig disk = 6;
    DisplayConfig display = 7;
    RenderBackend render_backend = 8;
    NetworkConfig network = 9;
}

message InstanceInfo {
    string id = 1;
    string name = 2;
    InstanceState state = 3;
    ResourceMetrics metrics = 4;
    repeated string tags = 5;
}
```

---

## 6. Управление ресурсами

### 6.1 Мониторинг метрик

**CPU:**
- Загрузка каждого ядра
- Приоритет процесса
- Привязка к ядрам

**Memory:**
- Использование RAM
- Ballooning (динамическое изменение)
- ZRAM (сжатие памяти)
- KSM (объединение страниц)

**GPU:**
- Использование VRAM
- FPS
- Загрузка GPU

**Disk:**
- Размер файла
- Занятое место
- Скорость чтения/записи
- Остаточное место

**Network:**
- Скорость загрузки/отдачи
- Количество пакетов
- Latency

### 6.1.1 Источники метрик (откуда `metrics_stream` берёт данные)

Метрики из §6.1 приходят из разных мест, не из одного API:

| Метрика | Источник |
|---|---|
| CPU-загрузка QEMU-процесса, RSS | `/proc/<pid>/stat`, `/proc/<pid>/status` на хосте (host-side, без участия гостя) |
| Точное использование RAM гостем, ballooning-статистика | QMP `query-balloon` / `BALLOON_CHANGE` events — требует включённого `virtio-balloon` устройства |
| ZRAM/KSM-эффективность | host-side: `/sys/kernel/mm/ksm/*`, статистика zram-устройства внутри гостя (для ZRAM — нужен guest-agent или Android-специфичный пробник) |
| Disk I/O | QMP `query-blockstats` |
| Network throughput | QMP `query-netdev` либо парсинг хостовых netns-интерфейсов |
| GPU VRAM/загрузка/FPS | Нет единого QMP-источника; на старте — недоступно или приблизительно (`hostmem` лимит как верхняя граница). Точные метрики GPU — открытый вопрос, см. §10 |

Практически это означает, что `metrics_stream` в `andler-qemu` — это объединение нескольких источников опроса (QMP polling + `/proc`), а не один вызов. Для метрик, специфичных для гостевой ОС (ZRAM-эффективность внутри Android), потребуется тонкий guest-agent — на первом этапе можно обойтись без него и показывать только host-side метрики.

### 6.2 Оптимизация ресурсов

**KSM (Kernel Samepage Merging):**
- Объединение одинаковых страниц памяти между инстансами
- Особенно эффективно для одинаковых Android-инстансов

**Ballooning:**
- Возврат неиспользованной RAM хосту
- Динамическое изменение в реальном времени

**ZRAM:**
- Сжатие памяти внутри гостевой ОС
- Позволяет запускать тяжёлые приложения на малом объёме RAM

**Linked Clones:**
- Общий базовый образ для нескольких инстансов
- Экономия места на диске

---

## 7. Тестирование

### 7.1 Юнит-тесты

**andler-core:**
- Переходы FSM (валидные/невалидные)
- Сериализация InstanceConfig
- Логика выбора backend'а в Auto-режиме

**andler-qemu:**
- Сборка cmdline для каждой комбинации RenderBackend/NetworkConfig
- Проверка точного набора флагов

**andler-disk:**
- Операции на временных qcow2-файлах через реальный qemu-img

**andler-store:**
- Миграции и CRUD на временной/in-memory sqlite

### 7.2 Интеграционные тесты

**Полный цикл:**
```
create → start → status=Running → pause → resume → stop → remove
```

**Использование лёгкого тестового Linux-образа** (Alpine/минимальный cloud-image)

### 7.3 E2E-тесты

**С реальным QEMU и /dev/kvm:**
- Полный сценарий Android-в-VM (полный гостевой образ)
- Проверка VirtIO-GPU/CPU-рендеринга
- Тесты clone/snapshot/compact на образах среднего размера

### 7.4 CI

- `cargo test --workspace` (без KVM-зависимых тестов) — на каждый PR
- Отдельный job с `/dev/kvm` — на merge в основную ветку
- `cargo clippy --workspace -- -D warnings` и `cargo fmt --check`

---

## 8. Дистрибуция и упаковка

### 8.1 Зависимости

**Системные:**
- `qemu-system-x86_64` — гипервизор
- `qemu-utils` — инструменты для работы с дисками
- `ovmf` — UEFI firmware для современных машин
- `pipewire` — аудиоподсистема
- `libvirt` (опционально) — для некоторых сетевых режимов

**Runtime:**
- `libssl`, `libz` — криптография и сжатие
- `libsqlite3` — для хранилища
- `libprotobuf` — для gRPC

### 8.2 Форматы дистрибуции

| Формат | Что упаковывает | Зависимости | Приоритет |
|---|---|---|---|
| AUR (PKGBUILD) | core + gui (раздельные пакеты) | qemu, через pacman | Высокий |
| .deb | core + gui (раздельные пакеты) | qemu-system-x86, qemu-utils | Высокий |
| .rpm | core + gui (раздельные пакеты) | qemu-kvm | Средний |
| AppImage | gui (+ опционально core) | qemu — проверяется при первом запуске | Средний |
| Flatpak | только gui | требует отдельного решения по доступу к KVM | Низкий |

### 8.3 Релизный пайплайн

1. Сборка и тест core
2. Сборка frontend (Tauri build для Linux)
3. Параллельные джобы упаковки:
   - AUR PKGBUILD
   - .deb через cargo-deb
   - .rpm через cargo-generate-rpm
   - AppImage через linuxdeploy
4. Публикация артефактов в GitHub Releases

---

## 9. Порядок реализации

1. **andler-core** — типы, trait HypervisorBackend, FSM, юнит-тесты
2. **andler-qemu** — сборка cmdline + супервизия + QMP-клиент
3. **andler-vmm** — пустой каркас: структура крейта и `impl HypervisorBackend for VmmBackend` присутствуют (чтобы реестр backend'ов в `andler-daemon` собирался и мог их перечислить), но каждый метод трейта (`spawn`, `pause`, `resume`, `stop`, `status`, `snapshot`, `metrics_stream`) возвращает `Err(BackendError::NotImplemented)` с понятным сообщением, а не `todo!()`/`unimplemented!()` — это нужно, чтобы demon не паниковал, если пользователь явно укажет `BackendKind::Vmm` до готовности backend'а, а получил штатную ошибку через gRPC
4. **andler-disk** — создание/clone/resize/compact
5. **andler-store** — sqlite-персистентность, миграции
6. **andler-net** — структуры конфигурации
7. **andler-rpc** + **andler-daemon** — связывание слоёв, реестр backend'ов
8. **andler-cli** — тонкий клиент, первые сквозные прогоны
9. Интеграционные тесты полного цикла
10. Docker (Dockerfile.dev, docker-compose)
11. guest-image пайплайн
12. Dockerfile.daemon + e2e-тесты

---

## 10. Открытые вопросы

- **gRPC vs более простой протокол** — gRPC выбран для стриминга и кросс-платформенности
- **Уровень A vs Уровень B для Android-слоя** — Уровень A (Waydroid) зафиксирован как стартовый
- **Нужен ли rootless-режим QEMU** — влияет на Docker-образ и ожидания по производительности. Базовое решение: `andlerd` не требует root для запуска QEMU при условии, что пользователь состоит в группе `kvm` (доступ к `/dev/kvm` через group permissions, без `CAP_SYS_ADMIN`). Пакеты `.deb`/`.rpm`/`AUR` должны проверять/предлагать добавление пользователя в группу `kvm` post-install — это должно быть явным шагом в packaging, а не предположением, что у пользователя уже есть права.
- **Момент, когда rust-vmm считается "достаточно зрелой"** — критерий для старта этапа rust-vmm backend
- **Версионирование gRPC/proto-схемы** — `andler-rpc` используется одновременно CLI, GUI и в перспективе companion-app; при параллельной разработке нужно зафиксировать политику совместимости (например, semver на `.proto`-пакет и запрет breaking changes без major-бампа), иначе CLI/GUI разных версий начнут расходиться с демоном без явной ошибки.
- **Метрики GPU (VRAM/загрузка/FPS)** — нет единого host-side источника данных через QMP (см. §6.1.1); нужно решить, использовать ли GPU-specific тулинг хоста (`nvidia-smi`/`radeontop`-подобные) как опциональный best-effort источник, или явно показывать "недоступно" на первом этапе.

---

## 11. Связанные документы

- **ANDLER_PRODUCT_CONCEPT.md** — продуктовая концепция, функционал
- **ANDLER_FRONTEND_PLAN.md** — архитектура GUI-клиента
- **ANDLER_MASTER_PLAN.md** — общий план проекта, упаковка, дистрибуция
