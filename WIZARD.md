# Plan: Wizard — Perfect before continuing

> All changes needed to make the wizard feature-complete.
> No code — only structure, decisions, and file mapping.

---

## What changes

1. **English wizard** — all prompts, summaries, help messages, errors in English
2. **ARM translator enum** — replace `bool libndk` with `{None, Libndk, Libhoudini}` across all layers
3. **Basic/Advanced in-wizard** — remove `--advanced` CLI flag, add first question in wizard
4. **Wizard decomposition** — split `wizard.rs` (546 lines) into 4 modules by complexity level
5. **Default labels** — every prompt shows its default value explicitly
6. **GPU metrics to firmware** — move `gpu_metrics.rs` (563 lines) from backend to `andler-firmware/src/metrics/`

---

## What NOT to change

Files that must NOT be modified during wizard implementation:

- `cli/src/lifecycle.rs` — start/stop/pause/resume/remove (unchanged)
- `cli/src/snapshot.rs` — snapshot actions (unchanged)
- `cli/src/clone.rs` — clone/export (unchanged)
- `cli/src/disk.rs` — disk actions (unchanged)
- `backends/andler-qemu/src/*` — QEMU backend (unchanged)
- `services/andler-disk/src/*` — disk service (unchanged)
- `services/andler-store/src/*` — store (unchanged)
- `services/andler-rpc/src/server.rs` — gRPC server (unchanged)
- `daemon/src/daemon/*.rs` — daemon core (except tests, unchanged)

**Restructured** (not unchanged):
- `services/andler-firmware/src/detect.rs` → **restructured** into `detect/ovmf.rs` (existing code preserved)

---

## UX flow (before → after)

### Before

```
andler create --advanced  → all questions (Advanced)
andler create             → only Basic (4 questions)
--libndk (bool flag)      → ARM translator on/off
All prompts in Russian
```

### After

```
andler create → wizard starts
  1.  Configuration mode: [Use recommended settings] / Customize all settings
      Basic  → questions 2-6 only
      Advanced → questions 2-21
  2.  VM type: [Linux] / Android
  3.  VM name
  4.  [Linux] Path to ISO image (Enter — skip, boot from disk)
      [Android] Base image path
  5.  [Android] Android version: [Android 13]
  6.  Disk size (GiB): [256]
  ── end of Basic ──
  7.  [Advanced] CD-ROM bus: [virtio-scsi (auto)] / ide
  8.  [Advanced] Compact disk after shutdown? [no]
  9.  [Advanced] GPU renderer: [Venus] / VirGL / VirtioGPU / CPU
  10. [Advanced] GPU memory (MiB): [4096]
  11. [Advanced] Display resolution: [1920x1080]
  12. [Advanced] Start in fullscreen? [no]
  13. [Advanced] Display refresh rate limit (0 = unlimited): [0]
  14. [Advanced] Audio backend: [PipeWire (auto)] / PulseAudio / None
  15. [Advanced] Enable clipboard sharing? [yes]
  16. [Advanced] Input pointer: [tablet] / mouse
  17. [Advanced] Network mode: [NAT (passt)] / Bridge / Isolated
  18. [Advanced] CPU cores: [4]
  19. [Advanced] Memory (GiB): [8]
  20. [Advanced, Android] ARM translator: [auto-detected] / libndk / libhoudini / none
  21. [Advanced, Android] GApps? [no] / MicroG? [no] / Root: [none]
  22. Summary + Confirm
```

All prompts in **English**. Defaults shown in brackets or `with_default()`.

### Navigation (keyboard shortcuts)

| Key | Action |
|-----|--------|
| ↑ / ↓ | Move between list items |
| Enter | Confirm current selection (or accept default for text input) |
| Space | Toggle current item (MultiSelect only) |
| ← / → | Deselect all / Select all (MultiSelect only) |
| Esc | Go back to previous question |
| Ctrl+C | Cancel wizard entirely, no files created |

See PLAN.md "Navigation" section for full details.

---

## Part 1: ARM translator enum

### Problem

`bool libndk` in code vs `libndk/libhoudini` in plan. Plan says: libndk for AMD, libhoudini for Intel. One active at a time.

### What to do

Replace `bool libndk` with `enum ArmTranslator { None, Libndk, Libhoudini }` across all layers:

| Layer | File | Change |
|-------|------|--------|
| Core domain | `core/andler-core/src/android_profile.rs` | New enum, replace field, update `cache_key()` |
| Proto | `services/andler-rpc/proto/andler.proto` | New enum message, replace `bool libndk = 4`, reserve field 4 |
| Convert | `services/andler-rpc/src/convert.rs` | Add `TryFrom` for new enum, update profile conversion |
| CLI flags | `cli/src/main.rs` | New `CliArmTranslator` enum (ValueEnum), replace `--libndk` |
| TOML | `cli/src/instance_file.rs` | `arm_translator: Option<String>` with backward compat for `libndk = true` |
| Create | `cli/src/create.rs` | `build_android_request` takes enum |
| Status | `cli/src/status.rs` | Print `libndk`/`libhoudini`/`none` |
| Tests | 5 test files | Replace all `libndk: true/false` |

### Auto-detection of ARM translator

When user doesn't specify `--arm-translator`, wizard auto-detects from CPU vendor:
- `/proc/cpuinfo` → `vendor_id`
- `AuthenticAMD` → `Libndk`
- `GenuineIntel` → `Libhoudini`
- Unknown → `None`

Detection happens once at wizard start. Result shown in prompt as `[libndk (auto)]` or `[libhoudini (auto)]`.

---

## Part 2: English wizard

### All prompts

| # | Russian (current) | English (target) |
|---|-------------------|------------------|
| 1 | `"Тип виртуальной машины:"` | `"VM type:"` |
| 2 | `"Имя VM:"` | `"VM name:"` |
| 3 | `"Имя не может быть пустым"` | `"Name cannot be empty"` |
| 4 | `"Имя не должно содержать слэши"` | `"Name must not contain slashes"` |
| 5 | `"Путь к ISO-образу (Enter — пропустить, загрузка с диска):"` | `"Path to ISO image (Enter — skip, boot from disk):"` |
| 6 | `"Путь к base-image Android:"` | `"Path to Android base image:"` |
| 7 | `"Путь к base-image обязателен"` | `"Base image path is required"` |
| 8 | `"Версия Android:"` | `"Android version:"` |
| 9 | `"Android 13 (рекомендуется)"` | `"Android 13 (recommended)"` |
| 10 | `"Носитель установки обнаружен... CD-ROM привод:"` | `"Installation media detected. CD-ROM bus:"` |
| 11 | `"Сжимать диск после выключения?"` | `"Compact disk after shutdown?"` |
| 12 | `"Рендер (GPU):"` | `"GPU render:"` |
| 13 | `"Движок дисплея:"` | `"Display engine:"` |
| 14 | `"Звуковое устройство:"` | `"Audio device:"` |
| 15 | `"Устройство ввода (указатель):"` | `"Input pointer:"` |
| 16 | `"Сетевой режим:"` | `"Network mode:"` |
| 17 | `"Количество ядер CPU:"` | `"CPU cores:"` |
| 18 | `"Объем памяти (GiB):"` | `"Memory (GiB):"` |
| 19 | `"ARM-транслятор:"` | `"ARM translator:"` |
| 20 | `"Установить GApps?"` | `"Enable GApps?"` |
| 21 | `"Установить MicroG?"` | `"Enable MicroG?"` |
| 22 | `"Root-режим:"` | `"Root mode:"` |
| 23 | `"none — без root; magisk — root через Magisk..."` | `"none — no root; magisk — root via Magisk (requires path to extracted binaries)"` |
| 24 | `"Путь к каталогу с бинарями Magisk:"` | `"Path to Magisk binaries directory:"` |
| 25 | `"Путь обязателен при root=magisk"` | `"Path is required when root=magisk"` |
| 26 | `"Размер диска (GiB):"` | `"Disk size (GiB):"` |
| 27 | `"Thin-provisioned qcow2..."` | `"Thin-provisioned qcow2 — nominal limit, not actual host usage"` |
| 28 | `"Введите целое число, например 256"` | `"Enter an integer, e.g. 256"` |
| 29 | `"Автоматически компактировать диск после каждого выключения?"` | `"Automatically compact disk after each shutdown?"` |
| 30 | `"Экономит место..."` | `"Saves space but rewrites entire disk file — may take time on large disks"` |
| 31 | `"Создать VM?"` | `"Create VM?"` |
| 32 | `"Память GPU (MiB):"` | `"GPU memory (MiB):"` |
| 33 | `"Разрешение экрана:"` | `"Display resolution (e.g. 1920x1080):"` |
| 34 | `"Запускать в полном экране?"` | `"Start in fullscreen mode?"` |
| 35 | `"Лимит частоты кадров (0 = безлимит):"` | `"Display refresh rate limit (0 = unlimited):"` |
| 36 | `"Аудио система:"` | `"Audio backend:"` |
| 37 | `"Включить обмен буфером обмена?"` | `"Enable clipboard sharing between host and VM?"` |

### Summary boxes

| Russian | English |
|---------|---------|
| `"┌─ Сводка перед созданием ────"` | `"┌─ Summary before creation ────"` |
| `"│  Тип:"` | `"│  Type:"` |
| `"│  Имя:"` | `"│  Name:"` |
| `"│  ISO: (без ISO — загрузка с диска)"` | `"│  ISO: (no ISO — boot from disk)"` |
| `"│  Диск:"` | `"│  Disk:"` |
| `"│  libndk:"` | `"│  ARM translator:"` |
| `"Создать VM"` | `"Create VM"` |
| `"Изменить"` | `"Modify"` |
| `"Отмена"` | `"Cancel"` |
| `"Отменено."` | `"Cancelled."` |
| `"wizard отменён"` | `"wizard cancelled"` |
| TTY error (lines 529-536) | Full English translation |

### Help messages

**Basic questions:**

| Question | Help message |
|----------|-------------|
| VM type | `"Linux — any distro with ISO; Android — Android with VirtIO-GPU and Waydroid"` |
| ISO path | `"Enter path to .iso file, or press Enter to skip (boot from existing disk)"` |
| Base image | `"Path to pre-built Android base image (.qcow2)"` |
| Android version | `"Android 13 has better VirtIO-GPU/Venus support"` |
| Disk size | `"Thin-provisioned qcow2 — nominal limit, not actual host usage"` |

**Advanced questions:**

| Question | Help message |
|----------|-------------|
| CD-ROM bus | `"virtio-scsi — faster, modern distro initrds support it; ide — compatible with Windows and any unknown ISO"` |
| Compact on shutdown | `"Saves space but rewrites entire disk file — may take time on large disks"` |
| GPU render | `"Venus — Vulkan 3D (fastest); VirGL — OpenGL 3D (broader); VirtioGPU — 2D; CPU — software"` |
| GPU memory | `"Host memory allocated for GPU device. 4096 MiB is sufficient for most workloads."` |
| Display resolution | `"Initial screen resolution. Format: WIDTHxHEIGHT (e.g. 1920x1080, 2560x1440)."` |
| Fullscreen | `"Start the VM window in fullscreen mode."` |
| FPS limit | `"Cap display refresh rate. 0 = unlimited. Set to 60 for battery saving or to reduce host GPU load."` |
| Audio backend | `"PipeWire — modern, recommended; PulseAudio — legacy; None — no audio"` |
| Clipboard | `"Enable copy-paste between host and VM via qemu-vdagent."` |
| Input pointer | `"tablet — absolute coordinates (recommended); mouse — relative coordinates"` |
| Network mode | `"NAT — VM gets internet via host (default); Bridge — VM on host network; Isolated — no external access"` |
| CPU cores | `"Number of virtual CPUs. Default 4 is sufficient for most use cases."` |
| Memory | `"RAM in GiB. Default 8 is sufficient for most use cases."` |
| ARM translator | `"libndk — for AMD CPUs (recommended for AMD); libhoudini — for Intel CPUs (recommended for Intel); none — no ARM app support"` |
| GApps | `"Google Play Store and Google services. Requires internet to set up on first boot."` |
| MicroG | `"Open-source Google Play replacement. No Google account needed."` |
| Root mode | `"none — no root; magisk — root via Magisk (requires path to extracted Magisk binaries)"` |

---

## Part 3: Basic/Advanced — in-wizard choice

### Remove from CLI

- Delete `--advanced` flag from `Command::Create` in `cli/src/main.rs`
- Remove `advanced` parameter from `create::handle()`
- Remove `advanced` parameter from `wizard::run()`

### Add to wizard

New enum and prompt at the very start of wizard:

```
enum WizardMode {
    Basic,    // questions 2-6 only
    Advanced, // questions 2-21
}
```

First question in wizard:
```
Configuration mode:
> Use recommended settings (Basic)
  Customize all settings (Advanced)

Basic — only essential questions (type, name, ISO, disk size).
Advanced — full control over GPU, GPU memory, resolution, fullscreen, FPS limit, audio, clipboard, input, network, CPU, memory, and more.
```

### Where each question appears

| # | Question | Basic | Advanced |
|---|----------|-------|----------|
| 1 | Configuration mode | Yes | — |
| 2 | VM type (Linux/Android) | Yes | Yes |
| 3 | VM name | Yes | Yes |
| 4 | ISO path (Linux) / Base image (Android) | Yes | Yes |
| 5 | Android version | Yes (Android only) | Yes |
| 6 | Disk size | Yes | Yes |
| 7 | CD-ROM bus | No | Yes (only if ISO provided) |
| 8 | Compact on shutdown | No | Yes |
| 9 | GPU renderer | No | Yes |
| 10 | GPU memory | No | Yes |
| 11 | Display resolution | No | Yes |
| 12 | Fullscreen | No | Yes |
| 13 | FPS limit | No | Yes |
| 14 | Audio backend | No | Yes |
| 15 | Clipboard sharing | No | Yes |
| 16 | Input pointer | No | Yes |
| 17 | Network mode | No | Yes |
| 18 | CPU cores | No | Yes |
| 19 | Memory | No | Yes |
| 20 | ARM translator (Android only) | No | Yes |
| 21 | GApps/MicroG/Root (Android only) | No | Yes |
| 22 | Summary + Confirm | Yes | Yes |

---

## Part 4: Wizard decomposition

### Current state

Single file: `cli/src/wizard.rs` — 546 lines.

### Target structure

Split by **complexity level** (Basic/Advanced), not by OS type. The Advanced questions 9-19 (GPU, GPU memory, resolution, fullscreen, FPS limit, audio, clipboard, input, network, CPU, memory) are **identical** for Linux and Android — only a few questions at the end (20-21) are Android-specific.

```
cli/src/wizard/
├── mod.rs        (~100 lines)
│   Entry point + orchestration + shared types
│
├── basic.rs      (~120 lines)
│   Core config questions (2-6)
│
├── advanced.rs   (~200 lines)
│   Hardware & deep config (7-21)
│
└── summary.rs    (~120 lines)
    Review screen + confirmation (22)
```

### mod.rs — Entry & Orchestration

Responsibilities:
- `pub async fn run(partial: PartialArgs) -> Result<WizardResult, WizardError>`
- `pub struct PartialArgs` (already-transferred CLI flags: `kind`, `name`, `iso_path`, `base_image_path`, `instances_root`, `quick`)
- Auto-detected values come from `andler-firmware::detect_all()` — wizard consumes, not detects
- `pub enum WizardResult { Linux(CreateInstanceRequest, String /* instances_root */), Android(CreateAndroidInstanceRequest) }`
- `pub enum WizardKind { Linux, Android }`
- `pub enum WizardMode { Basic, Advanced }`
- `pub enum WizardError` (NotTty, Cancelled, Inquire, Firmware(andler_firmware::FirmwareError)) — `Firmware` variant used only for OVMF VARS provisioning (provision_vars), not for detect_all (which is infallible)
- `fn is_tty() -> bool`
- `fn map_inquire_err(e: InquireError) -> WizardError`
- `fn ask_wizard_mode() -> Result<WizardMode, WizardError>` — first question
- `fn build_create_request(basic: &BasicResult, advanced: Option<&AdvancedConfig>, detected: &HardwareDefaults) -> CreateInstanceRequest` — assembles the final request from all collected values. When `advanced` is `None` (Basic mode), uses `reference_default()` for all config fields (CPU, memory, GPU, display, audio, network, input). For Android in Basic mode, uses `detected.arm_translator`.

**Type conversion in build_create_request:**
- `CliArmTranslator` → `ArmTranslator` (from andler-core): `Libndk` → `ArmTranslator::Libndk`, `Libhoudini` → `ArmTranslator::Libhoudini`, `None` → `ArmTranslator::None`
- `CliRootMode` → `RootMode` (from andler-core): `None` → `RootMode::None`, `Magisk` → `RootMode::Magisk`
- `AdvancedConfig.gapps`/`microg` → `AndroidProfile.gapps`/`microg` (direct bool copy)

**OVMF failure handling in build_create_request:**
- `detected.ovmf` is `Err`:
  - Linux → use Legacy BIOS (no firmware args passed to QEMU)
  - Android → error (should not reach here — OVMF failure caught in step 3 or by validation)

Flow:
```
run(partial):
  1. check TTY
  2. detected = andler_firmware::detect_all()     (infallible — returns HardwareDefaults)
      // detected: ovmf, gpu_render, display_engine, audio_server, arm_translator, venus_ok, passt_available
      // ovmf is Result<DetectedOvmf, FirmwareError> — check for Android (requires UEFI)
      // If GPU/ARM/audio detection fails → returns degraded values (Cpu, None, None)
      // Only OVMF can return Err
  3. if partial.quick → skip wizard, build request with all defaults (step 11)
  4. ask_wizard_mode() → Basic or Advanced
  5. basic::ask_kind(partial.kind) → Linux or Android
  6. basic::ask_name(partial.name) → name
  7. match kind:
     Linux  → basic_result = BasicResult::Linux(basic::run_linux(name, partial.iso_path, partial.instances_root))
     Android → basic_result = BasicResult::Android(basic::run_android(name, partial.base_image_path, partial.instances_root))
  8. advanced_config = match mode:
     Advanced → Some(advanced::run_*(basic_result, &detected, None))
     Basic    → None
  9. summary::run(&basic_result, advanced_config.as_ref(), &detected) → confirm
  10. if "Modify" → go to step 8 with current AdvancedConfig as prefilled (loop)
  11. if "Create" → build CreateInstanceRequest from basic + advanced + detected
  12. return CreateInstanceRequest + instances_root to create.rs
```

**Important:** `ask_kind()` and `ask_name()` are called in mod.rs, NOT inside `run_linux`/`run_android`. The `run_*` functions receive the already-collected name and only ask OS-specific questions (ISO path or base image).

### basic.rs — Core Config

Responsibilities:
- `pub fn run_linux(name: String, iso_path: Option<String>, instances_root: Option<String>) -> Result<LinuxBasicResult, WizardError>`
- `pub fn run_android(name: String, base_image_path: Option<String>, instances_root: Option<String>) -> Result<AndroidBasicResult, WizardError>`
- `pub fn ask_kind(prefilled: Option<WizardKind>) -> Result<WizardKind, WizardError>`
- `pub fn ask_name(prefilled: Option<String>) -> Result<String, WizardError>`
- `pub fn ask_iso_path(prefilled: Option<String>) -> Result<String, WizardError>` — returns empty string if user presses Enter (no ISO)
- `pub fn ask_base_image(prefilled: Option<String>) -> Result<String, WizardError>` — required for Android
- `pub fn ask_android_version() -> Result<CliAndroidVersion, WizardError>`
- `pub fn ask_disk_size(default_gib: u64) -> Result<u64, WizardError>` — validates input: must be integer ≥ 1, max 65536 GiB. Error: "Enter an integer between 1 and 65536, e.g. 256"

**Disk path auto-generation:** Disk path is NOT asked. It is auto-generated as `{name}-disk.qcow2` inside the instance directory (`{instances_root}/{instance_id}/`). The full path is shown in the summary.

**Note:** `ask_kind()` and `ask_name()` are called from mod.rs BEFORE calling `run_linux`/`run_android`. The `run_*` functions only ask OS-specific questions (ISO path or base image, Android version, disk size).

**Name validation rules:**
- Cannot be empty
- Must not contain `/` or `\` (path separators)
- Spaces, unicode, hyphens, underscores are allowed
- Leading/trailing whitespace is trimmed

Shared result types:
```rust
pub struct LinuxBasicResult {
    pub name: String,
    pub iso_path: String,
    pub disk_size_gib: u64,
    pub instances_root: String,
}

pub struct AndroidBasicResult {
    pub name: String,
    pub base_image: String,
    pub android_version: CliAndroidVersion,
    pub disk_size_gib: u64,
    pub instances_root: String,
}

pub enum BasicResult {
    Linux(LinuxBasicResult),
    Android(AndroidBasicResult),
}

impl BasicResult {
    pub fn name(&self) -> &str { match self { Self::Linux(r) => &r.name, Self::Android(r) => &r.name } }
    pub fn kind(&self) -> WizardKind { match self { Self::Linux(_) => WizardKind::Linux, Self::Android(_) => WizardKind::Android } }
    pub fn disk_size_gib(&self) -> u64 { match self { Self::Linux(r) => r.disk_size_gib, Self::Android(r) => r.disk_size_gib } }
    pub fn instances_root(&self) -> &str { match self { Self::Linux(r) => &r.instances_root, Self::Android(r) => &r.instances_root } }
}
```

### advanced.rs — Hardware & Deep Config

Responsibilities:
- `pub fn run_linux(result: LinuxBasicResult, detected: &HardwareDefaults, prefilled: Option<AdvancedConfig>) -> Result<AdvancedConfig, WizardError>`
- `pub fn run_android(result: AndroidBasicResult, detected: &HardwareDefaults, prefilled: Option<AdvancedConfig>) -> Result<AdvancedConfig, WizardError>`
- `fn ask_cdrom_bus(iso_name: &str, recommended: CdromBus, prefilled: Option<CdromBus>) -> Result<CdromBus, WizardError>`
- `fn ask_compact_on_shutdown(prefilled: Option<bool>) -> Result<bool, WizardError>`
- `fn ask_gpu_render(detected: &HardwareDefaults, prefilled: Option<RenderBackend>) -> Result<RenderBackend, WizardError>` — uses `detected.gpu_render` as default, `detected.venus_supported` to check Venus availability
- `fn ask_gpu_memory(prefilled: Option<u64>) -> Result<u64, WizardError>` — validates: must be 256-16384 MiB (256 MiB minimum for virtio-gpu, 16 GiB max reasonable)
- `fn ask_display_resolution(prefilled: Option<Resolution>) -> Result<Resolution, WizardError>` — validates: format must be `WIDTHxHEIGHT` (e.g. `1920x1080`), width/height 64-7680
- `fn ask_fullscreen(prefilled: Option<bool>) -> Result<bool, WizardError>`
- `fn ask_fps_limit(prefilled: Option<u32>) -> Result<u32, WizardError>` — validates: 0 (unlimited) or 1-240
- `fn ask_audio_backend(detected: &HardwareDefaults, prefilled: Option<AudioBackend>) -> Result<AudioBackend, WizardError>` — uses `detected.audio_server` as default
- `fn ask_clipboard_enabled(prefilled: Option<bool>) -> Result<bool, WizardError>`
- `fn ask_input_pointer(prefilled: Option<InputPointerMode>) -> Result<InputPointerMode, WizardError>`
- `fn ask_network_mode(prefilled: Option<NetworkMode>) -> Result<NetworkMode, WizardError>`
- `fn ask_cpu_cores(prefilled: Option<u32>) -> Result<u32, WizardError>` — validates: must be 1-128. Warns if exceeds host cores but allows it (QEMU handles overcommit)
- `fn ask_memory_gib(prefilled: Option<u64>) -> Result<u64, WizardError>` — validates: must be 1-1024. Warns if exceeds host RAM but allows it (host may have swap/zswap)
- `fn ask_arm_translator(detected: &HardwareDefaults, prefilled: Option<ArmTranslator>) -> Result<ArmTranslator, WizardError>` (Android only) — uses `detected.arm_translator` as default, shown in prompt as `[libndk (auto)]` or `[libhoudini (auto)]`
- `fn ask_gapps(prefilled: Option<bool>) -> Result<bool, WizardError>` (Android only) — Confirm prompt. If user selects yes, MicroG is automatically set to false (they're mutually exclusive)
- `fn ask_microg(prefilled: Option<bool>) -> Result<bool, WizardError>` (Android only) — Confirm prompt. If user selects yes, GApps is automatically set to false (they're mutually exclusive)
- `fn ask_root_mode(prefilled: Option<(CliRootMode, String)>) -> Result<(CliRootMode, String), WizardError>` (Android only)

**Loop support:** When user clicks "Modify" in summary, the wizard calls `run_linux`/`run_android` again with `detected: &HardwareDefaults` and `prefilled: Some(current_advanced_config)`. Each `ask_*` function checks if prefilled is `Some` — if yes, uses it as default and still shows the prompt (user can confirm by pressing Enter or change the value).

Shared result type:
```rust
pub struct AdvancedConfig {
    pub cdrom_bus: Option<CdromBus>,          // None if no ISO
    pub compact_on_shutdown: bool,
    pub gpu_render: RenderBackend,
    pub gpu_memory_mib: u64,                  // GPU memory in MiB
    pub display_resolution: Resolution,       // screen resolution
    pub fullscreen: bool,                     // start fullscreen
    pub fps_limit: u32,                       // 0 = unlimited
    pub audio_backend: AudioBackend,          // PipeWire/PulseAudio/None
    pub clipboard_enabled: bool,              // clipboard sharing
    pub input_pointer: InputPointerMode,
    pub network_mode: NetworkMode,
    pub cpu_cores: u32,
    pub memory_gib: u64,
    pub arm_translator: Option<CliArmTranslator>,  // Android only
    pub gapps: bool,                               // Android only, default false
    pub microg: bool,                              // Android only, default false
    pub root_mode: Option<(CliRootMode, String)>,  // Android only
}
```

OS-specific flow:
- `run_linux`: asks 7-19 (skips 20-21)
- `run_android`: asks 7-21 (all advanced questions)

### HardwareDefaults — auto-detected values from andler-firmware

**Target structure of `services/andler-firmware/src/`:**

```
services/andler-firmware/src/
├── lib.rs                  — pub API (detect_all, HardwareDefaults, detect, provision_vars, reset_vars)
├── error.rs                — FirmwareError enum (expanded)
└── detect/
    ├── mod.rs              — detect_all() orchestrator + HardwareDefaults struct
    ├── ovmf.rs             — existing code (renamed from detect.rs, unchanged logic)
    ├── gpu.rs              — detect_gpu_defaults() → (RenderBackend, DisplayEngine, bool)
    ├── arm.rs              — detect_arm_translator() → Option<ArmTranslator>
    ├── audio.rs            — detect_audio_server() → AudioServer
    └── network.rs          — detect_passt_available() → bool
```

All individual detect functions (`detect_gpu_defaults`, `detect_arm_translator`, `detect_audio_server`, `detect_passt_available`) are `pub(crate)` — only called by `detect_all()`. Only `detect_all()` and `HardwareDefaults` are `pub`.

`detect_all()` logs each step via `tracing::debug!` (e.g. "GPU detected: NVIDIA → Venus, SDL", "ARM translator: AMD CPU → libndk").

All checks are syscall-based (`Path::exists`, `std::process::Command`, file reads). No I/O blocking. Completes in <10ms. Results are NOT cached — each wizard invocation detects independently.

**HardwareDefaults struct:**

```rust
pub struct HardwareDefaults {
    pub ovmf: Result<DetectedOvmf, FirmwareError>,  // OVMF CODE+VARS paths
    pub gpu_render: RenderBackend,                    // Venus/VirGL/CPU (auto-detected)
    pub display_engine: DisplayEngine,                // SDL/GTK (auto-detected from GPU vendor)
    pub audio_server: AudioServer,                    // PipeWire/PulseAudio/None
    pub arm_translator: Option<ArmTranslator>,         // Libndk/Libhoudini/None (auto-detected from CPU, from andler-core)
    pub venus_supported: bool,                        // true if host meets Venus requirements
    pub passt_available: bool,                        // true if /usr/bin/passt or /usr/local/bin/passt found
}
```

This struct is populated once by `andler-firmware::detect_all()` and passed to `advanced::run_*()`, `summary::run()`, and `build_create_request()`. Wizard never detects hardware itself — it only consumes detected values.

### summary.rs — Review & Confirmation

Responsibilities:
- `pub fn run(basic: &BasicResult, advanced: Option<&AdvancedConfig>, detected: &HardwareDefaults) -> Result<SummaryAction, WizardError>`
- Displays full summary of ALL values (selected + defaults for unasked questions)
- Three actions: `Create` / `Modify` / `Cancel`
- `"Modify"` → returns `SummaryAction::Modify` to mod.rs, which loops back to advanced questions (step 8)
- **Basic params (kind, name, ISO/base image, disk size) cannot be changed via Modify.** If user wants to change them, they cancel and restart the wizard.
- When looping, current `AdvancedConfig` is passed as `prefilled` to `advanced::run_*()`. Each `ask_*` shows the current value and user can confirm or change it.

**Summary content for Basic mode:**
```
┌─ Summary before creation ─────────────────────────────────────┐
│  Type:              Linux VM                                   │
│  Name:              my-vm                                      │
│  ISO:               (no ISO — boot from disk)                  │
│  Disk:              ~/.local/share/andler/instances/<id>/      │
│                     my-vm-disk.qcow2 (256 GiB, qcow2)         │
│  Compact on shutdown: no (default)                             │
│  OVMF VARS:         auto-detected (/usr/share/edk2-ovmf/...)  │
│  GPU:               Venus, 4096 MiB (auto-detected)           │
│  Display:           1920x1080, windowed (default)              │
│  Audio:             PipeWire (auto-detected)                   │
│  Clipboard:         enabled (default)                          │
│  Network:           NAT/passt (default)                        │
│  CPU:               4 cores (default)                          │
│  Memory:            8 GiB (default)                            │
└───────────────────────────────────────────────────────────────┘
```

**When OVMF not found (Linux only):**
Summary shows: `│  OVMF VARS:         not found (Legacy BIOS will be used)  │` instead of the auto-detected path.

**Summary content for Advanced mode:** Same structure, but all values were explicitly chosen by the user (no "(default)" labels).

**Summary content for Android VM:**
```
┌─ Summary before creation ─────────────────────────────────────┐
│  Type:              Android VM                                 │
│  Name:              my-android                                 │
│  Base image:        /path/to/base.qcow2                       │
│  Android version:   13 (recommended)                           │
│  Disk:              ~/.local/share/andler/instances/<id>/      │
│                     my-android-disk.qcow2 (256 GiB, qcow2)    │
│  Compact on shutdown: no (default)                             │
│  UEFI:              OVMF VARS auto-detected                    │
│  ARM translator:    libndk (auto-detected: AMD CPU)            │
│  GApps:             no (default)                               │
│  MicroG:            no (default)                               │
│  Root:              none (default)                             │
│  GPU:               Venus, 4096 MiB (auto-detected)           │
│  Display:           1920x1080, windowed (default)              │
│  Audio:             PipeWire (auto-detected)                   │
│  Clipboard:         enabled (default)                          │
│  Network:           NAT (default)                              │
│  CPU:               4 cores (default)                          │
│  Memory:            8 GiB (default)                            │
└───────────────────────────────────────────────────────────────┘
```

**When OVMF not found (Android):**
Error raised before summary is shown — Android requires UEFI.

**Important:** Even in Basic mode, the summary shows ALL settings including GPU, display, audio, network, CPU, memory — values the user was never asked about. This is the only way the user can see what defaults will be applied before VM creation.

### Module visibility

- `mod.rs` — `pub` everything that `create.rs` imports
- `basic.rs`, `advanced.rs`, `summary.rs` — `pub(crate)` functions, called only from `mod.rs`

### Why this decomposition (not linux.rs/android.rs)

The Advanced questions 9-19 (GPU, GPU memory, resolution, fullscreen, FPS limit, audio, clipboard, input, network, CPU, memory) are **identical** for Linux and Android. Splitting by OS type would duplicate these ~120 lines. Splitting by complexity level keeps them in one place (`advanced.rs`) and only the last 2 questions (20-21) are Android-specific, handled by a simple `if kind == Android` branch inside `advanced.rs`.

---

## Part 5: Default labels

Every Select/Confirm/Text shows its default value explicitly:

| Question | Default | How it's shown |
|----------|---------|----------------|
| Wizard mode | Basic | First item in Select: `"Use recommended settings (Basic)"` |
| VM type | Linux | First item: `"Linux"` |
| Disk size | 256 | `with_default(256)` — shown in input field |
| Android version | 13 | First item: `"Android 13 (recommended)"` |
| CD-ROM bus | auto-detected | Prompt: `"CD-ROM bus (auto: {virtio-scsi/ide}):"` |
| Compact on shutdown | no | Confirm with `with_default(false)` |
| GPU render | Venus (if host meets requirements) | First item: `"Venus (3D via Vulkan, fastest)"` — from `detected.gpu_render` |
| GPU memory | 4096 MiB | `with_default(4096)` |
| Display resolution | 1920x1080 | `with_default("1920x1080")` |
| Fullscreen | no | Confirm with `with_default(false)` |
| FPS limit | 0 (unlimited) | `with_default(0)` — "0 = unlimited" |
| Audio backend | auto-detected | First item matches `detected.audio_server`: `"PipeWire (auto-detected)"` / `"PulseAudio (auto-detected)"` / `"None"` |
| Clipboard sharing | yes | Confirm with `with_default(true)` |
| Input pointer | tablet | First item: `"tablet (absolute coordinates, recommended)"` |
| Network mode | NAT | First item: `"NAT (passt, recommended)"` |
| CPU cores | 4 | `with_default(4)` |
| Memory | 8 GiB | `with_default(8)` |
| ARM translator | auto-detected | Prompt: `"ARM translator (auto: {libndk/libhoudini/none}):"` — from `detected.arm_translator` |
| GApps | no | Confirm with `with_default(false)` |
| MicroG | no | Confirm with `with_default(false)` |
| Root mode | none | First item: `"none"` |

### Default implementation details

For Advanced questions, defaults are implemented via `with_default()` or by placing the default item first in the Select list. All auto-detected values come from `HardwareDefaults` (populated by `andler-firmware::detect_all()`):

- GPU render: `Select::new(&options).with_default(&detected.gpu_render)` — auto-detected from host GPU vendor
- GPU memory: `Text::new(...).with_default("4096")` — 4 GiB, common default for most GPUs
- Display resolution: `Text::new(...).with_default("1920x1080")` — standard Full HD
- Fullscreen: `Confirm::new(...).with_default(false)` — windowed by default
- FPS limit: `Text::new(...).with_default("0")` — unlimited by default
- Audio: `Select::new(&options).with_default(&detected.audio_server)` — auto-detected from host PipeWire/PulseAudio sockets
- Clipboard: `Confirm::new(...).with_default(true)` — enabled by default (most users want copy-paste between host and VM)
- Network: `Select::new(&options).with_default(&"nat")` — first item
- CPU/Memory: `Text::new(...).with_default(&default.to_string())` — shown in input field (default: 4 cores, 8 GiB)

### Auto-detection of audio server

Implemented in `services/andler-firmware/src/detect/audio.rs`. Called once by `detect_all()`:

1. Check if PipeWire is running: `/run/user/{uid}/pipewire-0` socket
2. Check if PulseAudio is running: `/run/user/{uid}/pulse/native` socket
3. If `/run/user/{uid}/` doesn't exist (container, no session) → `AudioServer::None`
4. Neither socket found → `AudioServer::None`

Result stored in `HardwareDefaults.audio_server`. Summary shows: `"virtio-sound + PipeWire (default)"` or `"virtio-sound + PulseAudio (default)"`

### Auto-detection for passt (network)

Implemented in `services/andler-firmware/src/detect/network.rs`. Called once by `detect_all()`:

1. Check if `passt` binary exists: `/usr/bin/passt` or `/usr/local/bin/passt`
2. If found → `passt_available = true`
3. If not found → `passt_available = false`

Result stored in `HardwareDefaults.passt_available`. Network prompt default: "NAT (passt)" if available, "NAT" otherwise.

### Auto-detection for ARM translator

Implemented in `services/andler-firmware/src/detect/arm.rs`. Called once by `detect_all()`:

- Read `/proc/cpuinfo` for `vendor_id` field
- If file doesn't exist or `vendor_id` not found (container) → `None`
- `AuthenticAMD` → `Some(ArmTranslator::Libndk)`
- `GenuineIntel` → `Some(ArmTranslator::Libhoudini)`
- Unknown → `None`

Result stored in `HardwareDefaults.arm_translator`. Shown in prompt as `[libndk (auto)]` or `[libhoudini (auto)]`.

### Auto-detection for GPU vendor

Implemented in `services/andler-firmware/src/detect/gpu.rs`. Called once by `detect_all()`:

- Detect GPU vendor via `/sys/class/drm/card*/device/vendor` or `lspci`
- If multiple GPUs: prefer discrete (NVIDIA/AMD) over integrated (Intel)
- User can override in wizard if auto-detection picks wrong GPU
- AMD → `(RenderBackend::Venus, DisplayEngine::Gtk)`
- Intel → `(RenderBackend::Venus, DisplayEngine::Gtk)`
- NVIDIA → `(RenderBackend::Venus, DisplayEngine::Sdl)` (GTK fails on NVIDIA)
- No GPU → `(RenderBackend::Cpu, DisplayEngine::None)`
- Venus supported only if kernel ≥ 6.13, QEMU ≥ 9.2, Mesa ≥ 24.2
  Check methods:
  - Kernel: `uname -r`, parse first 3 version components (e.g. `6.13.0` → `6, 13, 0`). If `uname` not available → `venus_supported = false`
  - QEMU: `qemu-system-x86_64 --version`, parse version string (e.g. `QEMU emulator version 9.2.0`). If not available → `venus_supported = false`
  - Mesa: `glxinfo 2>/dev/null | grep "OpenGL version"`, parse version number (e.g. `4.6 (Compatibility Profile) Mesa 24.2.3`). If not available → `venus_supported = false`
  - GPU vendor: try `/sys/class/drm/card*/device/vendor` first, then `lspci` if sysfs empty. If both unavailable → `(RenderBackend::Cpu, DisplayEngine::None)`
  - If any version check fails → `venus_supported = false`, fallback to VirGL

Result stored in `HardwareDefaults.gpu_render`, `HardwareDefaults.display_engine`, `HardwareDefaults.venus_supported`.

---

## Part 6: TOML backward compatibility

Existing TOML files may have `libndk = true/false`. New field is `arm_translator: Option<String>`.

### Migration logic (in `instance_file.rs`)

```rust
// When reading TOML:
let arm_translator = toml.arm_translator
    .or_else(|| {
        // Backward compat: convert libndk bool to arm_translator string
        toml.libndk.map(|v| if v { "libndk".into() } else { "none".into() })
    });
```

- If `arm_translator` is set in TOML → use it
- Else if `libndk` is set → convert: `true` → `"libndk"`, `false` → `"none"`
- Else → `None` (use auto-detection or CLI flag)
- If both are set → `arm_translator` takes precedence

### Proto field

```protobuf
message AndroidProfile {
    // ...
    reserved 3;  // was root_policy (moved to RootMode)
    ArmTranslator arm_translator = 5;
    bool gapps = 6;
    bool microg = 7;
}
```

- Reserve field 4 (was `bool libndk`)
- New `ArmTranslator` enum at message level (not nested)

---

## Part 7: PLAN.md updates

Update the Wizard section (lines 240-395) in PLAN.md:

1. **Language**: Add note that wizard is English-only
2. **Basic/Advanced**: Remove `--advanced` flag references, add "first question in wizard"
3. **ARM translator**: Replace `--libndk` with `--arm-translator none/libndk/libhoudini`
4. **Auto-detection**: Centralized in `andler-firmware`, wizard consumes `HardwareDefaults`
5. **Interface examples**: Replace Russian examples with English
6. **Remove**: All references to `--advanced` flag
7. **Add**: `HardwareDefaults` struct to andler-firmware section

---

## Part 8: Metrics — GPU metrics to firmware

### Problem

GPU metrics (`gpu_metrics.rs`, 563 lines) live in `backends/andler-qemu/`, but GPU detection (`detect/gpu.rs`) lives in `andler-firmware`. Both read the same sysfs paths and vendor tools. Detection + monitoring should be in one place.

### What to do

Move GPU metrics from `backends/andler-qemu/src/gpu_metrics.rs` to `services/andler-firmware/src/metrics/`.

**Why:**
1. GPU detection + monitoring in one place (firmware)
2. GPU metrics are host-level — not tied to QEMU PID
3. Dependency backend → firmware already exists
4. Backend stays focused on VM lifecycle

**What stays in backend:**
- CPU%, RAM, disk I/O, network I/O — all per-VM, need PID

### Target structure

```
services/andler-firmware/src/
├── lib.rs                  — pub API (detect_all, HardwareDefaults, read_gpu_metrics, merge_gpu_metrics, ...)
├── error.rs                — FirmwareError enum
├── detect/
│   ├── mod.rs              — detect_all() orchestrator + HardwareDefaults struct
│   ├── ovmf.rs             — existing code (renamed from detect.rs)
│   ├── gpu.rs              — detect_gpu_defaults() → (RenderBackend, DisplayEngine, bool)
│   ├── arm.rs              — detect_arm_translator() → Option<ArmTranslator>
│   ├── audio.rs            — detect_audio_server() → AudioServer
│   └── network.rs          — detect_passt_available() → bool
└── metrics/
    ├── mod.rs              — pub fn read_gpu_metrics() + merge_gpu_metrics()
    ├── gpu_amd.rs          — AMD sysfs (mem_info_vram_*, gpu_busy_percent)
    ├── gpu_nvidia.rs       — nvidia-smi CLI
    └── gpu_intel.rs        — i915 sysfs (rc6_residency_ms delta)
```

### Public API

```rust
// andler-firmware/src/metrics/mod.rs

/// Detects GPU vendor (AMD → NVIDIA → Intel) and returns metrics.
pub fn read_gpu_metrics() -> ResourceMetrics;

/// Merges GPU fields from `gpu` into `base` (fills None fields).
pub fn merge_gpu_metrics(base: &mut ResourceMetrics, gpu: &ResourceMetrics);
```

### Changes in backend

**Before:**
```rust
// backends/andler-qemu/src/metrics.rs
let gpu = crate::gpu_metrics::read_gpu_metrics();
crate::gpu_metrics::merge_gpu_metrics(&mut metrics, &gpu);
```

**After:**
```rust
// backends/andler-qemu/src/metrics.rs
let gpu = andler_firmware::metrics::read_gpu_metrics();
andler_firmware::metrics::merge_gpu_metrics(&mut metrics, &gpu);
```

### Vendor-specific implementations

| Vendor | File | Data source | Metrics |
|--------|------|-------------|---------|
| AMD | `gpu_amd.rs` | `/sys/class/drm/card*/device/mem_info_vram_used`, `gpu_busy_percent` | VRAM used/total, GPU load % |
| NVIDIA | `gpu_nvidia.rs` | `nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv,noheader,nounits` | VRAM used/total, GPU load % |
| Intel | `gpu_intel.rs` | `/sys/class/drm/card*/device/power/rc6_residency_ms` (delta) | GPU load % only, no VRAM |

### Intel GPU load calculation

GPU load = `100% - (Δrc6_residency_ms / Δwall_clock_ms * 100)`

- `rc6_residency_ms`: cumulative milliseconds GPU spent in RC6 idle state since boot
- Uses real `Instant` wall-clock delta (not assumed 1s interval)
- First call returns `None` (no previous sample)
- Clamped to `[0, 100]` (rc6 accounting not perfectly synchronized with clock)

### What NOT to change

- `backends/andler-qemu/src/metrics.rs` — per-VM poller stays, but calls firmware for GPU
- `core/andler-core/src/backend.rs` — `ResourceMetrics` struct unchanged
- `daemon/src/daemon/query_ops.rs` — proxy unchanged
- `daemon/src/service.rs` — gRPC handler unchanged
- `cli/src/status.rs` — display unchanged

---

## File change map

| File | What changes |
|------|-------------|
| `services/andler-firmware/src/detect.rs` | **Rename** to `detect/ovmf.rs`, move existing code |
| `services/andler-firmware/src/detect/mod.rs` | **New**: `detect_all()` orchestrator, `HardwareDefaults` struct |
| `services/andler-firmware/src/detect/gpu.rs` | **New**: `detect_gpu_defaults()` → `(RenderBackend, DisplayEngine, bool /* venus_supported */)` |
| `services/andler-firmware/src/detect/arm.rs` | **New**: `detect_arm_translator()` → `Option<ArmTranslator>` |
| `services/andler-firmware/src/detect/audio.rs` | **New**: `detect_audio_server()` → `AudioServer` |
| `services/andler-firmware/src/detect/network.rs` | **New**: `detect_passt_available()` → `bool` |
| `services/andler-firmware/src/metrics/mod.rs` | **New**: `read_gpu_metrics()`, `merge_gpu_metrics()` |
| `services/andler-firmware/src/metrics/gpu_amd.rs` | **New**: AMD sysfs reads (mem_info_vram_*, gpu_busy_percent) |
| `services/andler-firmware/src/metrics/gpu_nvidia.rs` | **New**: nvidia-smi CLI |
| `services/andler-firmware/src/metrics/gpu_intel.rs` | **New**: i915 sysfs (rc6_residency_ms delta) |
| `services/andler-firmware/src/lib.rs` | Update exports: add `detect_all`, `HardwareDefaults` |
| `services/andler-firmware/src/error.rs` | Add `FirmwareError` variants only if individual detect functions can fail (OVMF already has `OvmfNotFound`; GPU/ARM/audio are infallible — return degraded values) |
| `services/andler-firmware/Cargo.toml` | No new dependencies needed — GPU uses sysfs/lspci, audio uses socket files, ARM uses /proc/cpuinfo |
| `core/andler-core/src/android_profile.rs` | New `ArmTranslator` enum, replace `libndk` field, update `cache_key()`, update tests |
| `services/andler-rpc/proto/andler.proto` | New enum, replace `bool libndk`, reserve field 4 |
| `services/andler-rpc/src/convert.rs` | Add conversion for `ArmTranslator`, update profile conversion, update tests |
| `cli/src/main.rs` | New `CliArmTranslator` enum, replace `--libndk`, remove `--advanced` |
| `cli/src/create.rs` | Remove `advanced` param, update `build_android_request` |
| `cli/src/instance_file.rs` | Replace `libndk: bool` with `arm_translator` (with backward compat), update tests + add backward compat tests |
| `cli/src/status.rs` | Print ARM translator name instead of bool (`libndk`/`libhoudini`/`none`) |
| `backends/andler-qemu/src/gpu_metrics.rs` | **Delete** (moved to `andler-firmware/src/metrics/`) |
| `backends/andler-qemu/src/metrics.rs` | Update: replace `crate::gpu_metrics` calls with `andler_firmware::metrics` |
| `cli/src/wizard.rs` | **Delete** (replaced by wizard/ directory) |
| `cli/src/wizard/mod.rs` | **New**: run(), PartialArgs, WizardResult, WizardMode, WizardError, ask_wizard_mode(), build_create_request() + unit tests |
| `cli/src/wizard/basic.rs` | **New**: ask_kind, ask_name, ask_iso_path, ask_base_image, ask_android_version, ask_disk_size + BasicResult enum + unit tests |
| `cli/src/wizard/advanced.rs` | **New**: ask_cdrom_bus, ask_compact, ask_gpu, ask_gpu_memory, ask_resolution, ask_fullscreen, ask_fps_limit, ask_audio, ask_clipboard, ask_input, ask_network, ask_cpu, ask_memory, ask_arm_translator, ask_gapps, ask_microg, ask_root_mode + unit tests |
| `cli/src/wizard/summary.rs` | **New**: summary screen + confirm + "Modify" flow + Android-specific summary (updated with new fields: GPU memory, resolution, fullscreen, FPS limit, audio backend, clipboard) |
| `cli/Cargo.toml` | `inquire` already present — no change needed |
| `daemon/src/daemon/tests/common.rs` | Replace `libndk: false` → `arm_translator: ArmTranslator::None` |
| `daemon/src/daemon/tests/clone.rs` | Replace 5 occurrences |
| `daemon/src/daemon/tests/create.rs` | Replace 3 occurrences |
| `PLAN.md` | Update Wizard section (remove --advanced, add English, update ARM translator) |

---

## Test plan

### Unit tests

| Test | File | What it tests |
|------|------|---------------|
| `test_detect_gpu_nvidia` | `services/andler-firmware/src/detect/gpu.rs` | Mock NVIDIA → `(Venus, Sdl)` |
| `test_detect_gpu_amd` | `services/andler-firmware/src/detect/gpu.rs` | Mock AMD → `(Venus, Gtk)` |
| `test_detect_gpu_intel` | `services/andler-firmware/src/detect/gpu.rs` | Mock Intel → `(Venus, Gtk)` |
| `test_detect_gpu_no_gpu` | `services/andler-firmware/src/detect/gpu.rs` | No GPU → `(Cpu, None)` |
| `test_detect_arm_amd` | `services/andler-firmware/src/detect/arm.rs` | Mock `AuthenticAMD` → `Some(Libndk)` |
| `test_detect_arm_intel` | `services/andler-firmware/src/detect/arm.rs` | Mock `GenuineIntel` → `Some(Libhoudini)` |
| `test_detect_arm_unknown` | `services/andler-firmware/src/detect/arm.rs` | Mock `GenericCPU` → `None` |
| `test_detect_audio_pipewire` | `services/andler-firmware/src/detect/audio.rs` | Mock PipeWire socket → `PipeWire` |
| `test_detect_audio_pulse` | `services/andler-firmware/src/detect/audio.rs` | Mock PulseAudio socket → `PulseAudio` |
| `test_detect_audio_none` | `services/andler-firmware/src/detect/audio.rs` | No sockets → `None` |
| `test_detect_passt_found` | `services/andler-firmware/src/detect/network.rs` | Mock `/usr/bin/passt` exists → `true` |
| `test_detect_passt_not_found` | `services/andler-firmware/src/detect/network.rs` | No passt binary → `false` |
| `test_detect_all_success` | `services/andler-firmware/src/detect/mod.rs` | All detected → `HardwareDefaults` |
| `test_detect_all_partial_failure` | `services/andler-firmware/src/detect/mod.rs` | Some failures → partial defaults |
| `test_read_gpu_metrics_amd` | `services/andler-firmware/src/metrics/gpu_amd.rs` | Mock AMD sysfs → VRAM + load |
| `test_read_gpu_metrics_nvidia` | `services/andler-firmware/src/metrics/gpu_nvidia.rs` | Mock nvidia-smi output → VRAM + load |
| `test_read_gpu_metrics_intel` | `services/andler-firmware/src/metrics/gpu_intel.rs` | Mock rc6_residency_ms delta → load |
| `test_read_gpu_metrics_no_gpu` | `services/andler-firmware/src/metrics/mod.rs` | No GPU → default (all None) |
| `test_merge_gpu_metrics` | `services/andler-firmware/src/metrics/mod.rs` | GPU fields fill None in base |
| `test_parse_arm_translator_valid` | `cli/src/wizard/advanced.rs` | `parse_arm_translator("libndk")` → `Some(CliArmTranslator::Libndk)` |
| `test_parse_arm_translator_invalid` | `cli/src/wizard/advanced.rs` | `parse_arm_translator("unknown")` → `None` |
| `test_parse_resolution_valid` | `cli/src/wizard/advanced.rs` | `parse_resolution("1920x1080")` → `Ok(Resolution { width: 1920, height: 1080 })` |
| `test_parse_resolution_invalid_format` | `cli/src/wizard/advanced.rs` | `parse_resolution("1920")` → `Err` |
| `test_parse_resolution_out_of_range` | `cli/src/wizard/advanced.rs` | `parse_resolution("8192x8192")` → `Err` |
| `test_parse_gpu_memory_valid` | `cli/src/wizard/advanced.rs` | `parse_gpu_memory("4096")` → `Ok(4096)` |
| `test_parse_gpu_memory_too_low` | `cli/src/wizard/advanced.rs` | `parse_gpu_memory("128")` → `Err` |
| `test_parse_fps_limit_valid` | `cli/src/wizard/advanced.rs` | `parse_fps_limit("60")` → `Ok(60)` |
| `test_parse_fps_limit_unlimited` | `cli/src/wizard/advanced.rs` | `parse_fps_limit("0")` → `Ok(0)` |
| `test_build_create_request_linux` | `cli/src/wizard/mod.rs` | `build_create_request(BasicResult::Linux(...), Some(advanced), &detected)` → correct request |
| `test_build_create_request_android` | `cli/src/wizard/mod.rs` | `build_create_request(BasicResult::Android(...), Some(advanced), &detected)` → correct request |
| `test_build_create_request_basic_mode` | `cli/src/wizard/mod.rs` | `build_create_request(basic, None, &detected)` → uses defaults |
| `test_build_create_request_advanced_clipboard` | `cli/src/wizard/mod.rs` | `build_create_request` with clipboard_enabled=false → correct field |
| `test_build_create_request_advanced_gpu_memory` | `cli/src/wizard/mod.rs` | `build_create_request` with gpu_memory_mib=8192 → correct field |
| `test_build_create_request_advanced_resolution` | `cli/src/wizard/mod.rs` | `build_create_request` with resolution=2560x1440 → correct field |
| `test_build_create_request_advanced_fullscreen` | `cli/src/wizard/mod.rs` | `build_create_request` with fullscreen=true → correct field |
| `test_build_create_request_advanced_fps_limit` | `cli/src/wizard/mod.rs` | `build_create_request` with fps_limit=60 → correct field |
| `test_basic_result_accessors` | `cli/src/wizard/basic.rs` | `BasicResult::Linux(...).name()` → correct name |
| `test_toml_backward_compat_libndk_true` | `cli/src/instance_file.rs` | TOML with `libndk = true` → `arm_translator: "libndk"` |
| `test_toml_backward_compat_libndk_false` | `cli/src/instance_file.rs` | TOML with `libndk = false` → `arm_translator: "none"` |
| `test_toml_arm_translator_overrides_libndk` | `cli/src/instance_file.rs` | TOML with both → `arm_translator` wins |

### Integration tests

| Test | What it tests |
|------|---------------|
| `test_wizard_basic_linux` | Full wizard flow: Basic mode → Linux → confirm → correct request |
| `test_wizard_advanced_linux` | Full wizard flow: Advanced mode → Linux → all questions → confirm |
| `test_wizard_android_with_arm_translator` | Full wizard flow: Advanced → Android → ARM translator selection |
| `test_wizard_modify_loop` | Summary → Modify → change GPU → Summary → confirm |
| `test_wizard_cancel_at_summary` | Summary → Cancel → no files created |
| `test_wizard_not_tty` | Non-TTY → error message → exit |
| `test_quick_linux_ovmf_not_found` | --quick + Linux + OVMF not found → Legacy BIOS warning, create request succeeds |
| `test_quick_android_ovmf_not_found` | --quick + Android + OVMF not found → error |
| `test_quick_android_base_image_not_found` | --quick + Android + base image not found → error |

### Running tests

```bash
# Unit tests only (no KVM)
docker compose -f docker/docker-compose.yml build --no-cache unit-test
docker compose -f docker/docker-compose.yml run --rm unit-test

# Full integration + unit tests (KVM required)
docker compose -f docker/docker-compose.yml build --no-cache integration-test
docker compose -f docker/docker-compose.yml run --rm integration-test
```

### Existing test updates (file changes table)

All test files that reference `libndk: true/false` must be updated to `arm_translator: ArmTranslator::Libndk`:

- `daemon/src/daemon/tests/common.rs` — 2 occurrences
- `daemon/src/daemon/tests/clone.rs` — 5 occurrences
- `daemon/src/daemon/tests/create.rs` — 3 occurrences
- `cli/src/instance_file.rs` — backward compat tests (add new, keep old with `libndk` field)
- `services/andler-firmware/src/detect/ovmf.rs` — existing tests (unchanged, just moved)
- `services/andler-firmware/src/detect/mod.rs` — new tests for `detect_all()`

---

## Edge cases & error handling

| Case | Behavior |
|------|----------|
| Not a TTY | Print English error: "Interactive wizard is not available (no TTY). Pass parameters via flags or use `--file <config.toml>`." Then exit. |
| `--file` + `--kind` | Error: "error: --file and --kind are mutually exclusive" (unchanged) |
| All CLI params provided | Skip wizard entirely, create directly (backward compat for scripts/CI) |
| `--file` TOML is incomplete | Error: "TOML file is missing required fields: {list}. Fix the file or use the wizard without --file." Wizard does NOT launch to fill gaps. |
| Disk already exists at path | Wizard asks: overwrite / choose different path / cancel |
| Instance directory already exists | Error: "Instance directory already exists: {path}. Use a different name or remove the existing instance first." |
| OVMF not found + Android VM | Error from `andler-firmware::FirmwareError::OvmfNotFound` with distro-specific install command. Android REQUIRES UEFI — no Legacy BIOS fallback. |
| OVMF not found + Linux VM | Warning: "OVMF not found. Legacy BIOS will be used. Install edk2-ovmf for UEFI support." Then continue. |
| Venus selected but host doesn't meet requirements | Warning before creation, suggest fallback to VirGL/CPU. `detected.venus_supported = false` from andler-firmware. |
| GTK selected on NVIDIA GPU | Warning: "GTK display may not work with NVIDIA proprietary drivers. Consider SDL or Spice." Auto-detected by andler-firmware (NVIDIA → SDL default). User can confirm or change. |
| `--root magisk` without `--magisk-dir` | Wizard asks for path explicitly, cannot continue without it |
| User presses Esc during wizard | Cancel, no files created |
| User presses Ctrl+C | Cancel, no files created |
| User clicks "Modify" in summary | Loop back to advanced questions with current values as prefilled defaults |
| Invalid disk size (float, negative, overflow) | Error message: "Enter an integer, e.g. 256" |
| Invalid resolution format | Error: "Invalid format. Use WIDTHxHEIGHT, e.g. 1920x1080" |
| Resolution out of range | Error: "Width and height must be between 64 and 7680" |
| Invalid GPU memory | Error: "Enter an integer between 256 and 16384, e.g. 4096" |
| Invalid FPS limit | Error: "Enter 0 (unlimited) or a value between 1 and 240" |
| Invalid CPU cores | Error: "Enter an integer between 1 and 128" |
| CPU cores exceeds host | Warning: "Host has {N} cores. VM will use {M} cores. Proceed?" |
| Invalid memory | Error: "Enter an integer between 1 and 1024, e.g. 8" |
| Memory exceeds host | Warning: "Host has {N} GiB RAM. VM will use {M} GiB. Proceed?" |
| No ISO + no existing disk (Linux) | Wizard allows this — VM will fail to boot, but wizard doesn't block it. Summary shows "(no ISO — boot from disk)". |
| ISO provided but file doesn't exist | Warning: "File not found: {path}. Continue anyway?" (wizard doesn't validate ISO existence) |
| Base image file doesn't exist | Error: "Base image not found: {path}. Android requires a valid base image." (wizard validates base image because it's required) |
| `--quick` flag | Skip wizard entirely, use all defaults, create VM directly (requires `--kind` to be specified) |
| `--quick` without `--kind` | Error: "`--quick` requires `--kind` to specify VM type" |
| `--quick` + `--file` | Error: "`--quick` and `--file` are mutually exclusive — --file already provides all configuration" |
| `--quick` + OVMF not found + Android | Error: "Android requires UEFI. Install edk2-ovmf and try again." |
| `--quick` + OVMF not found + Linux | Continue with Legacy BIOS warning: "OVMF not found. Legacy BIOS will be used. Install edk2-ovmf for UEFI support." |
| `--quick` + base image not found + Android | Error: "Base image not found: {path}. Android requires a valid base image." |
| Concurrent wizard instances | Not supported. Second wizard instance prints error: "Another wizard is already running. Wait for it to finish or use --file/--quick." |

---

## Execution order

1. **andler-firmware** restructure (this is the foundation — wizard depends on it):
   a. Rename `detect.rs` → `detect/ovmf.rs` (preserve existing code)
   b. Create `detect/gpu.rs` — `detect_gpu_defaults()` → `(RenderBackend, DisplayEngine, bool)`
   c. Create `detect/arm.rs` — `detect_arm_translator()` → `Option<ArmTranslator>`
   d. Create `detect/audio.rs` — `detect_audio_server()` → `AudioServer`
   e. Create `detect/network.rs` — `detect_passt_available()` → `bool`
   f. Create `detect/mod.rs` — `detect_all()` orchestrator + `HardwareDefaults` struct
   g. Update `lib.rs` — export new API
   h. Update `error.rs` — expand `FirmwareError` enum (GPU/ARM/audio are infallible — return degraded values, no new variants needed; only OVMF has `OvmfNotFound`)
   i. Update `Cargo.toml` — no new dependencies needed (verify existing deps are sufficient)
   j. Tests for all detect modules
   k. Create `metrics/mod.rs` — `read_gpu_metrics()`, `merge_gpu_metrics()`
   l. Create `metrics/gpu_amd.rs` — AMD sysfs reads
   m. Create `metrics/gpu_nvidia.rs` — nvidia-smi CLI
   n. Create `metrics/gpu_intel.rs` — i915 sysfs (rc6_residency_ms delta)
   o. Delete `backends/andler-qemu/src/gpu_metrics.rs` (moved to firmware)
   p. Update `backends/andler-qemu/src/metrics.rs` — replace `crate::gpu_metrics` with `andler_firmware::metrics`
   q. Tests for GPU metrics modules
2. Core domain: `ArmTranslator` enum + update `cache_key()`
3. Proto: new enum message + reserve field 4 + add fields 5-7
4. Convert: add `TryFrom` for `ArmTranslator`, update profile conversion
5. CLI main: `CliArmTranslator` enum (ValueEnum) + remove `--advanced` + add `--quick`
6. CLI instance_file: TOML `arm_translator` field + backward compat (Part 6)
7. CLI create: `build_android_request` takes enum + remove `advanced` param + handle `WizardResult`
8. CLI wizard/: decompose into wizard/ directory + English + Basic/Advanced + ARM translator + GPU memory + resolution + fullscreen + FPS limit + audio backend + clipboard + defaults + summary + loop
9. CLI status: display ARM translator name instead of bool
10. Tests: all `libndk: true/false` replacements + new unit tests (Part 5)
11. PLAN.md: update Wizard section (remove --advanced, add English, update ARM translator)
12. instance_file tests: update TOML fixtures + backward compat tests
13. Verify: all help messages in English, all defaults labeled, summary shows all values, `cargo test` passes

---

## Planned features (not in wizard, future work)

The following features are defined in the domain model but **not implemented in the backend**. They should NOT be added to the wizard until the backend supports them. Add wizard questions only after the backend wires them up to real QEMU flags.

### Ballooning (virtio-balloon)

**What:** Allow the host to dynamically reclaim unused RAM from the guest via `virtio-balloon-pci`.

**Status:** `MemoryConfig.ballooning` exists in `andler-core`, but `cmdline.rs` does not add `-device virtio-balloon-pci`. Dead field.

**To implement:** Add `ballooning: bool` → if true, append `-device virtio-balloon-pci` to QEMU args. Add QMP `query-balloon` for metrics.

### ZRAM (guest-internal)

**What:** Compressed swap inside the guest kernel. Reduces disk I/O for memory-heavy workloads.

**Status:** `MemoryConfig.zram` exists in `andler-core`, but ZRAM is a **guest-internal** mechanism — no QEMU flag. No code configures ZRAM inside the guest.

**To implement:** Requires SSH/exec into guest to run `modprobe zram`, `zramctl`, `mkswap`, `swapon`. Android-specific: Waydroid or init script.

### CPU priority (nice/ionice)

**What:** Set host process scheduling priority for the QEMU process via `nice`/`ionice`.

**Status:** `CpuConfig.priority` exists in `andler-core`, but `process.rs` does not call `nice()` or `ionice()` at spawn. Dead field.

**To implement:** Add `libc::nice(priority)` or `ionice` call in `QemuProcess::spawn()`. Map `CpuPriority::Low` → `nice(10)`, `Normal` → `nice(0)`, `High` → `nice(-10)`.

### FPS limit

**What:** Cap the VM's display refresh rate.

**Status:** `DisplayConfig.fps_limit` exists in `andler-core`, but `cmdline.rs` does not use it. QEMU does not have a native `fps=` flag for SDL.

**To implement:** Options: (a) QEMU spice streaming with fps limit, (b) guest-side VSync, (c) custom callback. Non-trivial — needs research.

### Network modes (Bridge, Isolated)

**What:** `NetworkMode::Bridge` and `NetworkMode::Isolated` are defined in `andler-core`.

**Status:** `cmdline.rs` panics on Bridge/Isolated — not implemented. Only NAT works.

**To implement:** Bridge needs `tap` device + `brctl`/`ip link`. Isolated needs `none` nic + nftables rules for inter-VM communication. Significant networking work.
